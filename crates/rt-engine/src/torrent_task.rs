/// Per-torrent async task.
///
/// One tokio task per torrent owns: tracker announce loop, peer connection
/// management, piece picker, and storage writes.
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::{SinkExt, StreamExt};
use reqwest::header::RANGE;
use rt_bencode::{decode, BValue};
use rusqlite::Connection;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, RwLock};
use tokio::time::{interval, sleep, timeout, Sleep};
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
use rt_piece_picker::{Availability, BlockRequest, PieceAvailability, PiecePicker, MAX_BLOCK_SIZE};
use rt_session::{SessionRegistry, TorrentState};
use rt_storage::{
    scheduler::{scheduled_read_owned, scheduled_write},
    IoClass, MountScheduler, PieceVerifier, SchedulerConfig, StorageIoConfig, VerifyResult,
};
#[cfg(test)]
use rt_tracker::TrackerError;
use rt_tracker::{TrackerEvent, TrackerState, TrackerStatus};
use rt_utp::UtpStream;

use crate::db_worker::DbExecutor;
use crate::egress_policy::{OutboundEgressPolicy, OutboundTargetKind};
use crate::network_budget::{GlobalNetworkBudget, SharedRateLimiter};
use crate::tracker_runtime::{
    announce_tracker, bounded_response_body, TrackerAnnounceContext, TrackerAnnounceResult,
    TrackerAnnounceSpec, TrackerWorkers, MAX_TRACKER_ANNOUNCES_IN_FLIGHT,
    STOPPED_TRACKER_ANNOUNCE_DEADLINE,
};
use crate::{EnginePeerSnapshot, EngineTorrentLimits, EngineWebseedSnapshot, TorrentRuntimeStats};

const LOCAL_UT_METADATA_ID: u8 = 1;
const LOCAL_UT_PEX_ID: u8 = 2;
const METADATA_PIECE_SIZE: usize = 16 * 1024;
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const PEER_UPLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PEER_SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_UPLOAD_REQUEST_WINDOW: Duration = Duration::from_secs(10);
// A normal client can issue one request per 16 KiB block. The previous 256
// request limit disconnected a healthy peer halfway through a 16 MiB
// transfer; keep the guard, but make it large enough for the 64 MiB local
// compatibility fixture and treat excess requests as bounded drops rather
// than a connection-fatal protocol error.
const MAX_PEER_UPLOAD_REQUESTS_PER_WINDOW: u32 = 4_096;
const MAX_PENDING_UPLOAD_READS: usize = 16;
// A 64 MiB fixture at the protocol's 16 KiB block size is 4,096 requests.
// Requests are only small coordinates; the response data remains bounded by
// MAX_PENDING_UPLOAD_READS and the peer-buffer governor.
const MAX_QUEUED_UPLOAD_REQUESTS: usize = 4_096;
const PEER_EVENT_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const WEBSEED_RETRY_BASE: Duration = Duration::from_secs(1);
const WEBSEED_RETRY_MAX: Duration = Duration::from_secs(300);
const MAX_UT_PEX_PEERS: usize = 2_048;

fn peer_event_channel_capacity(max_peers: usize) -> usize {
    max_peers.clamp(64, 512)
}

fn db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Packed piece availability. A `Vec<bool>` uses one allocation and one
/// byte-ish slot per piece on the hot peer path; this representation keeps
/// the exact piece count while using one bit per piece. It is intentionally
/// private to the engine because wire/API compatibility still uses ordinary
/// bool vectors at the protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PieceBitmap {
    len: usize,
    words: Vec<u64>,
}

impl PieceBitmap {
    fn new(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }

    fn from_bools(bits: &[bool]) -> Self {
        let mut bitmap = Self::new(bits.len());
        for (index, value) in bits.iter().copied().enumerate() {
            if value {
                bitmap.set(index, true);
            }
        }
        bitmap
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, index: usize) -> Option<bool> {
        (index < self.len).then(|| self.words[index / 64] & (1_u64 << (index % 64)) != 0)
    }

    fn set(&mut self, index: usize, value: bool) {
        if index >= self.len {
            return;
        }
        let word = &mut self.words[index / 64];
        let mask = 1_u64 << (index % 64);
        if value {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }

    fn count_ones(&self) -> usize {
        self.words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                let masked = if index + 1 == self.words.len() && !self.len.is_multiple_of(64) {
                    word & ((1_u64 << (self.len % 64)) - 1)
                } else {
                    *word
                };
                masked.count_ones() as usize
            })
            .sum()
    }

    fn first_set_u32(&self) -> Option<u32> {
        self.words.iter().enumerate().find_map(|(index, word)| {
            if *word == 0 {
                return None;
            }
            let piece = index
                .checked_mul(64)
                .and_then(|base| base.checked_add(word.trailing_zeros() as usize))?;
            u32::try_from(piece).ok()
        })
    }

    fn to_bitfield(&self) -> Vec<u8> {
        let mut bits = vec![0_u8; self.len.div_ceil(8)];
        for index in 0..self.len {
            if self.get(index).unwrap_or(false) {
                bits[index / 8] |= 0x80 >> (index % 8);
            }
        }
        bits
    }
}

impl PieceAvailability for PieceBitmap {
    fn has_piece(&self, piece: usize) -> bool {
        self.get(piece).unwrap_or(false)
    }
}

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
    ReloadFilePolicy {
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    UpdateLimits {
        limits: EngineTorrentLimits,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    UpdatePeerExchange(bool),
    /// TNG-002: stops network activity, disconnects every peer, and drains
    /// any already-buffered peer events so no further disk write can reach
    /// `handle_block` after the reply is sent. The caller must hold this
    /// guarantee for the entire duration of a storage move against this
    /// torrent's files -- without it, a peer write racing the move could
    /// write to a path mid-rename, or resurrect a file at the old path
    /// after the move already deleted it there. Replies with whether the
    /// torrent was already paused before this call, so the caller can
    /// restore that state afterward instead of unconditionally resuming.
    QuiesceForStorageMove {
        reply: oneshot::Sender<bool>,
    },
    /// TNG-002: re-points this task's cached `save_root` (and rebuilds the
    /// `MountScheduler` bound to it, so device-topology detection and any
    /// per-path handle-cache state is re-derived for the new location
    /// rather than staying pinned to the pre-move mount) after a storage
    /// move committed, then resumes activity unless the torrent was
    /// already paused before the move began. `new_save_root` is `None`
    /// when the move failed and rolled back -- the task simply resumes
    /// unchanged in that case.
    ResumeAfterStorageMove {
        new_save_root: Option<PathBuf>,
        resume_paused: bool,
    },
    Shutdown,
    /// Peers discovered by DHT.
    NewPeers(Vec<SocketAddr>),
    /// Peers explicitly added through a client API.
    PriorityPeers(Vec<SocketAddr>),
    /// Remove a peer immediately after the engine-wide ban policy admits it.
    /// Admission checks prevent future connections; this command closes an
    /// already-connected session and releases its piece/request state too.
    BanPeer(SocketAddr),
    GetPeers {
        reply: oneshot::Sender<Vec<EnginePeerSnapshot>>,
    },
    GetWebseeds {
        reply: oneshot::Sender<Vec<EngineWebseedSnapshot>>,
    },
    GetRuntimeStats {
        reply: oneshot::Sender<TorrentRuntimeStats>,
    },
    /// An inbound TCP peer whose handshake already matched this torrent.
    AcceptPeer {
        stream: TcpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    },
    /// An inbound uTP peer whose handshake already matched this torrent.
    AcceptUtpPeer {
        stream: UtpStream,
        peer_addr: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
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

#[cfg(test)]
fn stricter_limit(torrent_limit: Option<u64>, global_limit: Option<u64>) -> Option<u64> {
    match (torrent_limit, global_limit) {
        (Some(torrent), Some(global)) => Some(torrent.min(global)),
        (Some(limit), None) | (None, Some(limit)) => Some(limit),
        (None, None) => None,
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
        dropped: Vec<SocketAddr>,
    },
}

#[derive(Debug)]
struct PeerHandle {
    id: PeerId,
    cmd_tx: mpsc::Sender<PeerCommand>,
    /// Out-of-band cancellation for a peer whose command queue may be full.
    /// Control messages such as a ban must not depend on an untrusted peer
    /// draining a bounded command queue.
    abort: Option<tokio::task::AbortHandle>,
    peer_has: PieceBitmap,
    choked: bool,
    upload_choked: bool,
    interested: bool,
    downloaded: u64,
    uploaded: u64,
    download_rate: f64,
    upload_rate: f64,
    download_rate_window: u64,
    upload_rate_window: u64,
    download_rate_window_started: Instant,
    upload_rate_window_started: Instant,
    outstanding: usize,
    requested: Vec<BlockRequest>,
    ut_metadata_id: Option<u8>,
    ut_pex_id: Option<u8>,
    metadata_size: Option<u32>,
    _peer_permit: OwnedSemaphorePermit,
    /// HAVE messages that could not enter the bounded peer mailbox. The
    /// bitmap keeps this retry state bounded by the torrent's piece map and
    /// avoids losing a protocol update when a peer is briefly backlogged.
    pending_have: PieceBitmap,
    /// Latest upload limit not yet delivered to the peer. `Some(None)` is a
    /// pending explicit limit removal; `None` means there is no pending
    /// update.
    pending_upload_limit: Option<Option<u64>>,
}

#[derive(Debug)]
enum PeerCommand {
    Request(BlockRequest),
    Have(u32),
    Choke,
    Unchoke,
    UpdateUploadLimit(Option<u64>),
    Shutdown,
}

#[derive(Clone)]
struct UploadContext {
    save_root: PathBuf,
    // TNG-014: shared, not owned per peer -- PieceMap's `files: Vec<FileSpan>`
    // scales with file count, and a fresh deep clone on every new peer
    // connection (`upload_context()` below) was real, avoidable per-peer
    // memory and allocation cost for torrents with many files, at swarm
    // scale. `PieceMap` is only ever read after construction (never
    // mutated), so an `Arc` clone here is a cheap refcount bump instead.
    piece_map: Arc<PieceMap>,
    storage: MountScheduler,
    resources: ResourceGovernor,
    have_pieces: PieceBitmap,
    metadata: Option<Arc<Vec<u8>>>,
    is_private: bool,
    pex_enabled: bool,
    upload_limit_bytes_per_sec: Option<u64>,
    global_download: Arc<SharedRateLimiter>,
    global_upload: Arc<SharedRateLimiter>,
}

struct LeasedUploadBlock {
    data: bytes::Bytes,
    _lease: MemoryLease,
}

#[derive(Debug, Clone, Copy)]
struct UploadRequest {
    piece: u32,
    begin: u32,
    length: u32,
}

type UploadReadResult = (UploadRequest, anyhow::Result<LeasedUploadBlock>);

pub struct TorrentTask {
    info_hash_hex: String,
    meta: TorrentMetaV1,
    save_root: PathBuf,
    piece_map: Arc<PieceMap>,
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
    db: DbExecutor,
    resources: ResourceGovernor,
    network_budget: GlobalNetworkBudget,
    cmd_rx: mpsc::Receiver<TorrentCmd>,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_event_rx: mpsc::Receiver<PeerEvent>,
    peer_disconnect_tx: mpsc::Sender<PeerEvent>,
    peer_disconnect_rx: mpsc::Receiver<PeerEvent>,
    tracker_workers: TrackerWorkers,
    picker: PiecePicker,
    choker: Choker,
    /// active peer addresses
    active_peers: HashMap<SocketAddr, PeerHandle>,
    known_tracker_peers: HashSet<SocketAddr>,
    allowed_private_peers: HashSet<SocketAddr>,
    last_peerless_reannounce: Option<Instant>,
    egress_policy: OutboundEgressPolicy,
    webseed_next_index: usize,
    webseed_failures: Vec<u8>,
    webseed_next_attempt: Vec<Option<Instant>>,
    webseed_last_rates: Vec<i64>,
    webseed_last_success: Vec<Option<Instant>>,
    last_progress_persist: Option<Instant>,
    transfer_stats_dirty: bool,
    piece_assemblies: HashMap<u32, PieceAssembly>,
    piece_assembly_bytes: usize,
    piece_assembly_soft_cap_bytes: usize,
    piece_assembly_evictions: u64,
    peer_request_window_reductions: u64,
    peer_command_queue_full: u64,
    tracker_peer_cache_drops: u64,
    dirty_pieces_since_barrier: HashSet<u32>,
    super_seeding: bool,
    seed_ratio_limit: Option<f64>,
    seed_idle_limit: Option<Duration>,
    seeding_started_at: Option<Instant>,
    last_upload_at: Instant,
    download_limit_bytes_per_sec: Option<u64>,
    download_tokens: u64,
    download_tokens_updated: Instant,
    upload_limit_bytes_per_sec: Option<u64>,
    torrent_download_limit_bytes_per_sec: Option<u64>,
    torrent_upload_limit_bytes_per_sec: Option<u64>,
    completed_piece_verify_from_memory: u64,
    completed_piece_verify_from_disk: u64,
    prepared_files: Mutex<HashSet<u32>>,
    paused: bool,
    max_peers: usize,
    torrent_max_peers: Option<usize>,
    pex_enabled: bool,
    event_retention: usize,
}

impl TorrentTask {
    // This constructor is the current dependency-injection seam for a
    // torrent actor. It is intentionally explicit while the actor context is
    // being split into storage/network/persistence components.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        meta: TorrentMetaV1,
        save_root: PathBuf,
        paused: bool,
        registry: Arc<RwLock<SessionRegistry>>,
        db: DbExecutor,
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
        pex_enabled: bool,
        egress_policy: OutboundEgressPolicy,
        network_budget: GlobalNetworkBudget,
        event_retention: usize,
    ) -> Self {
        // Peer events are bounded per active torrent. Derive the capacity from
        // the daemon-wide peer ceiling so a deployment that deliberately runs
        // with a small peer budget does not retain 512 message slots per hot
        // torrent, while still keeping enough burst room for a normal
        // handshake/block event sequence. The upper bound prevents an
        // accidentally huge max_peers setting from multiplying memory.
        let peer_event_capacity = peer_event_channel_capacity(max_peers);
        let (peer_event_tx, peer_event_rx) = mpsc::channel(peer_event_capacity);
        let (peer_disconnect_tx, peer_disconnect_rx) = mpsc::channel(peer_event_capacity);
        let total = meta.total_length();
        let last_piece_len = if total.is_multiple_of(meta.piece_length) {
            meta.piece_length
        } else {
            total % meta.piece_length
        };
        let piece_count = meta.pieces.len();
        let webseed_failures = vec![0; meta.webseeds.len()];
        let webseed_next_attempt = vec![None; meta.webseeds.len()];
        let webseed_last_rates = vec![0; meta.webseeds.len()];
        let webseed_last_success = vec![None; meta.webseeds.len()];
        let picker = PiecePicker::new(piece_count, meta.piece_length as u32, last_piece_len as u32);
        let info_hash_hex: String = meta.info_hash.iter().map(|b| format!("{b:02x}")).collect();
        let piece_map = Arc::new(
            PieceMap::new(
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
            .expect("metainfo parser rejects invalid piece maps"),
        );
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
        let mut tracker_tiers = tracker_tiers_from_meta(&meta);
        let tracker_rows = db
            .run("restore_tracker_state", {
                let info_hash = info_hash_hex.clone();
                move |db| {
                    rt_db::list_torrent_trackers(db, &info_hash).map_err(|error| error.to_string())
                }
            })
            .await;
        match tracker_rows {
            Ok(rows) => restore_tracker_state_from_rows(&mut tracker_tiers, &rows),
            Err(error) => {
                warn!(
                    component = "db",
                    operation = "restore_tracker_state",
                    torrent = %info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to restore durable tracker state; using fresh tracker session"
                );
            }
        }
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
            network_budget,
            cmd_rx,
            peer_event_tx,
            peer_event_rx,
            peer_disconnect_tx,
            peer_disconnect_rx,
            tracker_workers: TrackerWorkers::new(),
            picker,
            choker: Choker::new(DEFAULT_MAX_UNCHOKED),
            active_peers: HashMap::new(),
            known_tracker_peers: HashSet::new(),
            allowed_private_peers: HashSet::new(),
            last_peerless_reannounce: None,
            egress_policy,
            webseed_next_index: 0,
            webseed_failures,
            webseed_next_attempt,
            webseed_last_rates,
            webseed_last_success,
            last_progress_persist: None,
            transfer_stats_dirty: false,
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
            super_seeding: false,
            seed_ratio_limit: None,
            seed_idle_limit: None,
            seeding_started_at: None,
            last_upload_at: Instant::now(),
            download_limit_bytes_per_sec: None,
            download_tokens: u64::MAX,
            download_tokens_updated: Instant::now(),
            upload_limit_bytes_per_sec: None,
            torrent_download_limit_bytes_per_sec: None,
            torrent_upload_limit_bytes_per_sec: None,
            completed_piece_verify_from_memory: 0,
            completed_piece_verify_from_disk: 0,
            prepared_files: Mutex::new(HashSet::new()),
            paused,
            max_peers,
            torrent_max_peers: None,
            pex_enabled,
            event_retention,
        };
        if let Err(error) = task.apply_torrent_limits_from_db().await {
            // Starting with an unbounded picker after a persisted-policy
            // read failed is unsafe: a damaged or unavailable database must
            // not silently widen the set of pieces we may write.
            task.fail_closed_runtime_policy("restore_runtime_policy", &error);
        }
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
        let mut peer_control_retry_tick = interval(Duration::from_secs(1));
        // Webseeds are a fallback path, not the primary piece scheduler. A
        // fixed interval would still wake every task even while it is paused,
        // complete, peer-connected, or webseed-free. A deadline-driven sleep
        // wakes only when the guarded work can run; after a failure it follows
        // that seed's exponential retry deadline.
        let mut webseed_sleep = Box::pin(sleep(WEBSEED_RETRY_MAX));
        reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());

        loop {
            tokio::select! {
                command = self.cmd_rx.recv() => {
                    let Some(cmd) = command else {
                        // The owning engine is gone. Persist the last safe
                        // state and close peer tasks instead of leaving a
                        // detached torrent actor running on timers forever.
                        warn!(
                            component = "torrent",
                            operation = "run",
                            torrent = %self.info_hash_hex,
                            result = "command_channel_closed",
                            "torrent command channel closed; shutting down"
                        );
                        self.announce_stopped().await;
                        self.persist_progress().await;
                        self.save_fastresume(false).await;
                        self.cancel_tracker_announces();
                        self.shutdown_peers().await;
                        break;
                    };
                    match cmd {
                        TorrentCmd::Shutdown => {
                            self.announce_stopped().await;
                            self.persist_progress().await;
                            self.save_fastresume(false).await;
                            self.cancel_tracker_announces();
                            self.shutdown_peers().await;
                            break;
                        }
                        TorrentCmd::Pause => {
                            self.paused = true;
                            self.cancel_tracker_announces();
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
                        TorrentCmd::QuiesceForStorageMove { reply } => {
                            let was_paused = self.paused;
                            self.paused = true;
                            self.cancel_tracker_announces();
                            self.announce_stopped().await;
                            self.shutdown_peers().await;
                            self.save_fastresume(false).await;
                            self.set_state(TorrentState::Paused).await;
                            self.tracker_event = TrackerEvent::Started;
                            // `shutdown_peers` above terminates every peer
                            // task, so no *new* PeerEvent can arrive after
                            // it returns -- but an event already buffered
                            // in the channel a moment before disconnect
                            // could still be sitting there. Drop anything
                            // left so a leftover Block event can't reach
                            // `handle_block` (and write to disk) after we
                            // hand back this reply.
                            while self.peer_event_rx.try_recv().is_ok() {}
                            let _ = reply.send(was_paused);
                        }
                        TorrentCmd::ResumeAfterStorageMove {
                            new_save_root,
                            resume_paused,
                        } => {
                            if let Some(new_root) = new_save_root {
                                self.save_root = new_root.clone();
                                self.storage = MountScheduler::new_for_path(
                                    StorageRootId::new(),
                                    &new_root,
                                    &SchedulerConfig {
                                        profile: StorageProfile::Unknown,
                                        resources: Some(self.resources.clone()),
                                        storage_io: self.storage.io_config().clone(),
                                        ..Default::default()
                                    },
                                );
                                // Any file-prepared bookkeeping refers to
                                // handles/allocations at the old path; the
                                // files now live at a verified-identical
                                // new path, so start clean rather than
                                // trust stale state across the move.
                                self.prepared_files.lock().expect("prepared_files mutex poisoned").clear();
                            }
                            if !resume_paused {
                                self.paused = false;
                                self.restart_tracker_session();
                                if matches!(self.run_recheck(None).await, RecheckOutcome::Shutdown)
                                {
                                    break;
                                }
                            }
                        }
                        TorrentCmd::NewPeers(addrs) => {
                            if !self.paused {
                                if !self.meta.private {
                                    self.remember_tracker_peers(&addrs);
                                }
                                self.connect_peers(addrs, PeerSource::Dht).await;
                            }
                        }
                        TorrentCmd::PriorityPeers(addrs) => {
                            if !self.paused {
                                self.remember_tracker_peers(&addrs);
                                self.connect_priority_peers(addrs).await;
                            }
                        }
                        TorrentCmd::BanPeer(peer) => {
                            self.evict_peer(peer);
                        }
                        TorrentCmd::GetPeers { reply } => {
                            let _ = reply.send(self.peer_snapshots());
                        }
                        TorrentCmd::GetWebseeds { reply } => {
                            let _ = reply.send(self.webseed_snapshots());
                        }
                        TorrentCmd::GetRuntimeStats { reply } => {
                            let _ = reply.send(self.runtime_stats());
                        }
                        TorrentCmd::AcceptPeer {
                            stream,
                            peer_addr,
                            handshake,
                            peer_permit,
                        } => {
                            if !self.paused {
                                self.accept_peer(stream, peer_addr, handshake, peer_permit)
                                    .await;
                            }
                        }
                        TorrentCmd::AcceptUtpPeer {
                            stream,
                            peer_addr,
                            handshake,
                            peer_permit,
                        } => {
                            if !self.paused {
                                self.accept_utp_peer(stream, peer_addr, handshake, peer_permit)
                                    .await;
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
                        TorrentCmd::ReloadFilePolicy { reply } => {
                            let result = self.apply_file_policy_from_db().await;
                            if let Err(error) = &result {
                                warn!(
                                    component = "torrent",
                                    operation = "reload_file_policy",
                                    torrent = %self.info_hash_hex,
                                    result = "error",
                                    error = %error,
                                    "failed to reload persisted file policy; retaining the active policy"
                                );
                            }
                            if let Some(reply) = reply {
                                let _ = reply.send(result);
                            }
                        }
                        TorrentCmd::UpdateLimits { limits, reply } => {
                            let result = self.apply_torrent_limits(&limits).await;
                            if let Err(error) = &result {
                                warn!(
                                    component = "torrent",
                                    operation = "update_limits",
                                    torrent = %self.info_hash_hex,
                                    result = "error",
                                    error = %error,
                                    "failed to apply persisted torrent limits"
                                );
                            }
                            if let Some(reply) = reply {
                                let _ = reply.send(result);
                            }
                        }
                        TorrentCmd::UpdatePeerExchange(enabled) => {
                            self.pex_enabled = enabled;
                        }
                    }
                }

                Some(event) = self.peer_event_rx.recv() => {
                    self.handle_peer_event(event).await;
                }

                Some(event) = self.peer_disconnect_rx.recv() => {
                    self.handle_peer_event(event).await;
                    if self.active_peers.is_empty() {
                        reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                    }
                }

                Some(result) = self.tracker_workers.recv() => {
                    self.handle_tracker_result(result).await;
                }

                _ = choke_tick.tick() => {
                    self.run_choker().await;
                }

                _ = tracker_tick.tick() => {
                    self.evict_banned_peers().await;
                    self.enforce_seed_limits().await;
                    if self.transfer_stats_dirty {
                        self.persist_progress().await;
                    }
                    if !self.paused {
                        self.start_due_tracker_announces().await;
                    }
                }

                _ = peer_retry_tick.tick() => {
                    if !self.paused {
                        self.retry_known_tracker_peers().await;
                    }
                }

                _ = peer_control_retry_tick.tick() => {
                    self.retry_pending_peer_controls();
                }

                _ = &mut webseed_sleep, if !self.paused && self.active_peers.is_empty() && !self.meta.webseeds.is_empty() && !self.picker.is_complete() => {
                    self.download_next_webseed_block().await;
                    reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                }
            }
        }
    }

    fn webseed_wake_delay(&self) -> Duration {
        if self.paused
            || !self.active_peers.is_empty()
            || self.meta.webseeds.is_empty()
            || self.picker.is_complete()
        {
            return WEBSEED_RETRY_MAX;
        }

        let now = Instant::now();
        let mut earliest_retry = WEBSEED_RETRY_MAX;
        let mut has_ready_seed = false;
        for (failures, next_attempt) in self
            .webseed_failures
            .iter()
            .zip(self.webseed_next_attempt.iter())
        {
            if *failures == u8::MAX {
                continue;
            }
            match next_attempt {
                Some(deadline) => {
                    earliest_retry = earliest_retry.min(deadline.saturating_duration_since(now));
                }
                None => has_ready_seed = true,
            }
        }
        if has_ready_seed {
            Duration::from_millis(100)
        } else {
            earliest_retry
        }
    }

    async fn connect_peers(&mut self, addrs: Vec<SocketAddr>, source: PeerSource) {
        for addr in addrs {
            if self.active_peers.len() >= self.peer_capacity() {
                break;
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
            let peer_cmd_rx = self.register_peer(addr, peer_permit);
            let peer_event_tx = self.peer_event_tx.clone();
            let peer_disconnect_tx = self.peer_disconnect_tx.clone();
            let upload = self.upload_context(addr);
            let transport_policy = outgoing_transport_policy_for_peer(
                outgoing_transport_policy_configured(),
                source,
                self.meta.private,
            );
            let peer_task = tokio::spawn(async move {
                let result = run_outgoing_peer_with_policy(
                    addr,
                    info_hash,
                    peer_event_tx,
                    peer_cmd_rx,
                    upload,
                    transport_policy,
                )
                .await;
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
                let _ = peer_disconnect_tx
                    .send(PeerEvent::Disconnected {
                        peer: addr,
                        outstanding: Vec::new(),
                    })
                    .await;
            });
            self.attach_peer_abort(addr, peer_task.abort_handle());
        }
    }

    async fn connect_priority_peers(&mut self, addrs: Vec<SocketAddr>) {
        let preferred: HashSet<SocketAddr> = addrs.iter().copied().collect();
        for addr in addrs {
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
    fn evict_peer(&mut self, peer: SocketAddr) -> bool {
        let Some(handle) = self.active_peers.remove(&peer) else {
            return false;
        };
        let bitfield = handle.peer_has.to_bitfield();
        self.picker.availability.remove_bitfield(&bitfield);
        for req in handle.requested {
            self.picker.cancel_request(req.piece as usize, req.begin);
        }
        let _ = handle.cmd_tx.try_send(PeerCommand::Shutdown);
        if let Some(abort) = handle.abort {
            abort.abort();
        }
        if self.active_peers.is_empty() {
            self.clear_piece_assemblies();
        }
        true
    }

    async fn start_due_tracker_announces(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }

        let tier_idx = self.active_tracker_tier.min(self.tracker_tiers.len() - 1);
        let available = self.tracker_workers.available();
        if available == 0 {
            return;
        }
        let candidates = self.tracker_tiers[tier_idx]
            .iter()
            .enumerate()
            .filter(|(idx, tracker)| {
                tracker.is_due() && !self.tracker_workers.contains((tier_idx, *idx))
            })
            .take(available)
            .map(|(idx, tracker)| TrackerAnnounceSpec {
                key: (tier_idx, idx),
                url: tracker.url.clone(),
                tracker_id: tracker.tracker_id.clone(),
                event: self.tracker_event,
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }

        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let context = self.tracker_announce_context(uploaded, downloaded);
        self.tracker_workers.start(candidates, context);
    }

    async fn handle_tracker_result(&mut self, result: TrackerAnnounceResult) {
        self.tracker_workers.complete(result.key, result.generation);
        if !self.tracker_workers.is_current(result.generation) {
            return;
        }
        let (tier_idx, tracker_idx) = result.key;
        let Some(tier) = self.tracker_tiers.get_mut(tier_idx) else {
            return;
        };
        let Some(tracker) = tier.get_mut(tracker_idx) else {
            return;
        };
        match result.response {
            Ok(resp) => {
                let peers: Vec<SocketAddr> = resp.peers.iter().map(|peer| peer.addr).collect();
                tracker.on_success(&resp);
                if let Some(scrape) = result.scrape {
                    tracker.scrape_complete = Some(scrape.complete);
                    tracker.scrape_incomplete = Some(scrape.incomplete);
                    tracker.scrape_downloaded = Some(scrape.downloaded);
                }
                if let Some(min_interval) = self.min_announce_interval {
                    if tracker.interval < min_interval {
                        tracker.interval = min_interval;
                    }
                }
                self.persist_tracker_state().await;
                self.tracker_event = tracker_event_after_success(self.tracker_event, result.event);
                if !peers.is_empty() && !self.paused {
                    self.remember_tracker_peers(&peers);
                    info!(
                        torrent = %self.info_hash_hex,
                        tracker = %result.url,
                        peers = peers.len(),
                        "tracker announce returned peers"
                    );
                    self.connect_peers(peers, PeerSource::Tracker).await;
                }
            }
            Err(err) => {
                warn!(
                    component = "tracker",
                    operation = "announce",
                    torrent = %self.info_hash_hex,
                    tracker = %result.url,
                    result = "error",
                    error = %err,
                    "tracker announce failed"
                );
                tracker.on_failure(err);
                self.persist_tracker_state().await;
            }
        }
        self.maybe_advance_tracker_tier(tier_idx);
    }

    fn tracker_announce_context(&self, uploaded: u64, downloaded: u64) -> TrackerAnnounceContext {
        TrackerAnnounceContext {
            info_hash: self.meta.info_hash,
            uploaded,
            downloaded,
            left: self.picker.bytes_left(),
            listen_port: self.listen_port,
            http_timeout: self.http_timeout,
            udp_timeout: self.udp_timeout,
            numwant: self.peer_capacity() as u32,
            egress_policy: self.egress_policy,
        }
    }

    fn maybe_advance_tracker_tier(&mut self, tier_idx: usize) {
        if tier_idx != self.active_tracker_tier || self.tracker_tiers[tier_idx].is_empty() {
            return;
        }
        let has_inflight = self.tracker_workers.has_inflight_tier(tier_idx);
        if !has_inflight
            && self.tracker_tiers[tier_idx]
                .iter()
                .all(|tracker| matches!(&tracker.status, TrackerStatus::Error(_)))
        {
            self.advance_tracker_tier();
        }
    }

    async fn announce_stopped(&mut self) {
        if !consume_stopped_announce(&mut self.stopped_announced) {
            return;
        }

        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let context = self.tracker_announce_context(uploaded, downloaded);
        let candidates = self
            .tracker_tiers
            .iter()
            .enumerate()
            .flat_map(|(tier_idx, tier)| {
                tier.iter().enumerate().map(move |(tracker_idx, tracker)| {
                    (
                        tier_idx,
                        tracker_idx,
                        tracker.url.clone(),
                        tracker.tracker_id.clone(),
                    )
                })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }

        // A stopped announce is still awaited so pause/quiesce/shutdown do
        // not report completion before the terminal tracker event has had a
        // chance to leave the process. The network work itself is bounded
        // and parallel, however: a dead tracker must not hold the actor for
        // one full timeout per tier entry. The outer deadline also cancels
        // requests whose individual HTTP/UDP timeout is longer.
        let mut pending = futures::stream::iter(candidates.into_iter().map(
            |(tier_idx, tracker_idx, url, tracker_id)| {
                let context = context.clone();
                async move {
                    let result = announce_tracker(
                        &context,
                        &url,
                        TrackerEvent::Stopped,
                        tracker_id.as_deref(),
                    )
                    .await;
                    (tier_idx, tracker_idx, url, result)
                }
            },
        ))
        .buffer_unordered(MAX_TRACKER_ANNOUNCES_IN_FLIGHT);
        let deadline = sleep(STOPPED_TRACKER_ANNOUNCE_DEADLINE);
        tokio::pin!(deadline);
        let mut results = Vec::new();
        let mut deadline_exceeded = false;
        loop {
            tokio::select! {
                result = pending.next() => {
                    let Some(result) = result else { break };
                    results.push(result);
                }
                _ = &mut deadline => {
                    deadline_exceeded = true;
                    break;
                }
            }
        }
        if deadline_exceeded {
            warn!(
                component = "tracker",
                operation = "announce_stopped",
                torrent = %self.info_hash_hex,
                result = "deadline_exceeded",
                deadline_secs = STOPPED_TRACKER_ANNOUNCE_DEADLINE.as_secs(),
                completed = results.len(),
                "stopped tracker announces exceeded aggregate deadline"
            );
        }

        let had_results = !results.is_empty();
        for (tier_idx, tracker_idx, url, result) in results {
            let Some(tracker) = self
                .tracker_tiers
                .get_mut(tier_idx)
                .and_then(|tier| tier.get_mut(tracker_idx))
            else {
                continue;
            };
            match result {
                Ok(resp) => tracker.on_success(&resp),
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
                    tracker.on_failure(err);
                }
            }
        }
        if had_results {
            self.persist_tracker_state().await;
        }
    }

    fn restart_tracker_session(&mut self) {
        self.cancel_tracker_announces();
        self.tracker_event = TrackerEvent::Started;
        self.stopped_announced = false;
    }

    fn cancel_tracker_announces(&mut self) {
        self.tracker_workers.cancel();
    }

    async fn accept_peer(
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
        let peer_cmd_rx = self.register_peer(peer_addr, peer_permit);
        let peer_event_tx = self.peer_event_tx.clone();
        let peer_disconnect_tx = self.peer_disconnect_tx.clone();
        let upload = self.upload_context(peer_addr);
        let peer_task = tokio::spawn(async move {
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
            }
            let _ = peer_disconnect_tx
                .send(PeerEvent::Disconnected {
                    peer: peer_addr,
                    outstanding: Vec::new(),
                })
                .await;
        });
        self.attach_peer_abort(peer_addr, peer_task.abort_handle());
    }

    async fn accept_utp_peer(
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
        let peer_cmd_rx = self.register_peer(peer_addr, peer_permit);
        let peer_event_tx = self.peer_event_tx.clone();
        let peer_disconnect_tx = self.peer_disconnect_tx.clone();
        let upload = self.upload_context(peer_addr);
        let peer_task = tokio::spawn(async move {
            if let Err(e) = run_incoming_utp_peer(
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
                    operation = "run_incoming_utp",
                    peer = %peer_addr,
                    result = "ended",
                    error = %e,
                    "incoming uTP peer ended"
                );
            }
            let _ = peer_disconnect_tx
                .send(PeerEvent::Disconnected {
                    peer: peer_addr,
                    outstanding: Vec::new(),
                })
                .await;
        });
        self.attach_peer_abort(peer_addr, peer_task.abort_handle());
    }

    fn upload_context(&self, peer_addr: SocketAddr) -> UploadContext {
        let have_pieces = self.picker.have_pieces();
        let visible_pieces = if self.super_seeding && self.picker.is_complete() {
            super_seed_visible_pieces(&have_pieces, peer_addr)
        } else {
            have_pieces
        };
        UploadContext {
            save_root: self.save_root.clone(),
            piece_map: self.piece_map.clone(),
            storage: self.storage.clone(),
            resources: self.resources.clone(),
            have_pieces: PieceBitmap::from_bools(&visible_pieces),
            metadata: torrent_info_bytes(&self.meta.raw).ok().map(Arc::new),
            is_private: self.meta.private,
            pex_enabled: self.pex_enabled,
            upload_limit_bytes_per_sec: self.upload_limit_bytes_per_sec,
            global_download: self.network_budget.download(),
            global_upload: self.network_budget.upload(),
        }
    }

    fn register_peer(
        &mut self,
        addr: SocketAddr,
        peer_permit: OwnedSemaphorePermit,
    ) -> mpsc::Receiver<PeerCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        self.active_peers.insert(
            addr,
            PeerHandle {
                id: PeerId::new(),
                cmd_tx,
                abort: None,
                peer_has: PieceBitmap::new(self.meta.pieces.len()),
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
                pending_have: PieceBitmap::new(self.meta.pieces.len()),
                pending_upload_limit: None,
            },
        );
        cmd_rx
    }

    fn attach_peer_abort(&mut self, addr: SocketAddr, abort: tokio::task::AbortHandle) {
        if let Some(peer) = self.active_peers.get_mut(&addr) {
            peer.abort = Some(abort);
        } else {
            // The peer can finish before the engine processes its first
            // event. Do not leave the just-created task alive in that race.
            abort.abort();
        }
    }

    fn peer_snapshots(&self) -> Vec<EnginePeerSnapshot> {
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
            .collect()
    }

    fn webseed_snapshots(&self) -> Vec<EngineWebseedSnapshot> {
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
                    .saturating_add(
                        (peer.peer_has.words.capacity() * std::mem::size_of::<u64>()) as u64,
                    )
            })
            .sum::<u64>();
        let tracker_peer_cache_bytes = (self.known_tracker_peers.capacity() as u64)
            .saturating_mul(std::mem::size_of::<SocketAddr>() as u64);
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
        if !self.try_consume_download_tokens(req.length) {
            self.picker.cancel_request(req.piece as usize, req.begin);
            return;
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
            let started = Instant::now();
            match self.fetch_webseed_block(&url, req).await {
                Ok(data) => {
                    let elapsed = started.elapsed().as_secs_f64().max(0.001);
                    let rate = (data.len() as f64 / elapsed).round() as i64;
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
                    // Charge the aggregate budget for bytes actually
                    // received. Failed seeds and short/error responses do
                    // not consume the process-wide download allowance.
                    self.network_budget
                        .download()
                        .acquire(data.len() as u64)
                        .await;
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
        let start = req.piece as u64 * self.meta.piece_length + req.begin as u64;
        let end = start + req.length as u64 - 1;
        let response = client
            .get(url.clone())
            .header(RANGE, format!("bytes={start}-{end}"))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}", response.status());
        }
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
                    handle.peer_has = PieceBitmap::from_bools(&pieces);
                }
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Have { peer, piece } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    if !handle.peer_has.get(piece as usize).unwrap_or(false) {
                        handle.peer_has.set(piece as usize, true);
                        self.picker.availability.add_have(piece as usize);
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
                    record_peer_transfer(handle, false, block.data.len() as u64);
                }
                self.handle_block(block).await;
                self.refill_peer_requests(peer).await;
            }
            PeerEvent::Uploaded { peer, bytes } => {
                if let Some(handle) = self.active_peers.get_mut(&peer) {
                    record_peer_transfer(handle, true, bytes);
                }
                self.record_upload(bytes).await;
            }
            PeerEvent::Disconnected { peer, outstanding } => {
                // The dedicated terminal channel normally carries an empty
                // request list. Recover the requests tracked by the engine
                // in that case; otherwise a saturated ordinary event queue
                // would strand picker reservations forever.
                let outstanding = if outstanding.is_empty() {
                    self.active_peers
                        .get(&peer)
                        .map(|handle| handle.requested.clone())
                        .unwrap_or_default()
                } else {
                    outstanding
                };
                if let Some(handle) = self.active_peers.get(&peer) {
                    let bitfield = handle.peer_has.to_bitfield();
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
            PeerEvent::PeerExchange {
                peer,
                peers,
                dropped,
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
                    self.allowed_private_peers.remove(&dropped_peer);
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
            if let Some(decision) = decisions.get(&handle.id).copied() {
                let (upload_choked, delivery_failed) =
                    Self::try_apply_choke_decision(&handle.cmd_tx, handle.upload_choked, decision);
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
    fn try_apply_choke_decision(
        tx: &mpsc::Sender<PeerCommand>,
        currently_choked: bool,
        decision: ChokeDecision,
    ) -> (bool, bool) {
        let (desired, command) = match decision {
            ChokeDecision::Unchoke if currently_choked => (false, PeerCommand::Unchoke),
            ChokeDecision::Choke if !currently_choked => (true, PeerCommand::Choke),
            _ => return (currently_choked, false),
        };
        match tx.try_send(command) {
            Ok(()) => (desired, false),
            Err(_) => (currently_choked, true),
        }
    }

    async fn refill_peer_requests(&mut self, peer: SocketAddr) {
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
        // Persist progress periodically so amount_left stays current even
        // before the first piece verifies. The picker tracks partial piece
        // bytes so progress is visible as blocks arrive.
        self.persist_progress_throttled(false).await;
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

    fn retry_pending_peer_controls(&mut self) {
        let mut queue_full = 0u64;
        for handle in self.active_peers.values_mut() {
            if let Some(limit) = handle.pending_upload_limit {
                match handle
                    .cmd_tx
                    .try_send(PeerCommand::UpdateUploadLimit(limit))
                {
                    Ok(()) => handle.pending_upload_limit = None,
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

    fn try_send_have(
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

    fn try_consume_download_tokens(&mut self, bytes: u32) -> bool {
        self.refill_download_tokens();
        let Some(limit) = self.download_limit_bytes_per_sec else {
            return true;
        };
        let bytes = u64::from(bytes);
        if self.download_tokens < bytes {
            return false;
        }
        self.download_tokens = self.download_tokens.saturating_sub(bytes);
        self.download_tokens = self.download_tokens.min(limit);
        true
    }

    fn refill_download_tokens(&mut self) {
        let now = Instant::now();
        let Some(limit) = self.download_limit_bytes_per_sec else {
            self.download_tokens = u64::MAX;
            self.download_tokens_updated = now;
            return;
        };
        let elapsed = now.saturating_duration_since(self.download_tokens_updated);
        self.download_tokens_updated = now;
        let refill = (elapsed.as_secs_f64() * limit as f64).floor() as u64;
        self.download_tokens = self.download_tokens.saturating_add(refill).min(limit);
    }

    async fn shutdown_peers(&mut self) {
        let handles: Vec<(mpsc::Sender<PeerCommand>, Option<tokio::task::AbortHandle>)> = self
            .active_peers
            .values()
            .map(|peer| (peer.cmd_tx.clone(), peer.abort.clone()))
            .collect();

        for (tx, abort) in handles {
            let _ = tx.try_send(PeerCommand::Shutdown);
            if let Some(abort) = abort {
                abort.abort();
            }
        }
        self.active_peers.clear();
        self.clear_piece_assemblies();
    }

    /// A ban update can race a full torrent command queue. Re-check the
    /// authoritative policy on a timer so an active connection is eventually
    /// evicted even if the best-effort control message was not enqueued.
    async fn evict_banned_peers(&mut self) {
        let banned = self.registry.read().await.banned_peers();
        if banned.is_empty() || self.active_peers.is_empty() {
            return;
        }
        let banned = banned.into_iter().collect::<HashSet<_>>();
        let victims = self
            .active_peers
            .keys()
            .copied()
            .filter(|peer| banned.contains(peer))
            .collect::<Vec<_>>();
        for peer in victims {
            self.evict_peer(peer);
        }
    }

    async fn record_download(&mut self, bytes: u64) {
        self.update_transfer(bytes, false).await;
    }

    async fn record_upload(&mut self, bytes: u64) {
        self.last_upload_at = Instant::now();
        self.update_transfer(bytes, true).await;
        self.enforce_seed_limits().await;
    }

    async fn update_transfer(&mut self, bytes: u64, upload: bool) {
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

    async fn enforce_seed_limits(&mut self) {
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
        self.paused = true;
        self.announce_stopped().await;
        self.shutdown_peers().await;
        self.save_fastresume(false).await;
        self.set_state(TorrentState::Paused).await;
        self.tracker_event = TrackerEvent::Started;
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

    async fn transfer_snapshot(&self) -> (u64, u64) {
        let reg = self.registry.read().await;
        reg.get(&self.info_hash_hex)
            .map(|entry| (entry.stats.uploaded, entry.stats.downloaded))
            .unwrap_or((0, 0))
    }

    async fn persist_tracker_state(&self) {
        if let Err(error) = self.persist_tracker_state_inner().await {
            warn!(
                component = "db",
                operation = "persist_tracker_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist tracker state; retaining the prior registry projection"
            );
        }
    }

    async fn persist_tracker_state_inner(&self) -> Result<(), String> {
        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let left = db_i64(self.picker.bytes_left());
        let now = Instant::now();
        let mut rows = Vec::new();
        let mut tracker_index = 0i64;
        // Native-engine counterpart to the sidecar's cached `t.message`
        // column: a torrent can be actively seeding/downloading fine while
        // its tracker rejects announces, which `state` alone never
        // reflects. Cache the first error/warning message found across
        // any tier on the registry entry so list/facet queries can read it
        // directly instead of needing a per-torrent round trip through
        // this actor.
        let mut tracker_message: Option<String> = None;
        for (tier_idx, tier) in self.tracker_tiers.iter().enumerate() {
            for tracker in tier {
                if tracker_message.is_none() {
                    tracker_message = tracker_failure_reason(&tracker.status)
                        .or_else(|| tracker_warning_message(&tracker.status));
                }
                rows.push(rt_db::TorrentTrackerRow {
                    info_hash: self.info_hash_hex.clone(),
                    tracker_index,
                    tier: i64::try_from(tier_idx).unwrap_or(i64::MAX),
                    url: tracker.url.clone(),
                    tracker_id: tracker.tracker_id.clone(),
                    status: tracker_status_label(&tracker.status).to_owned(),
                    last_announce_at: instant_to_unix(tracker.last_announce, now),
                    next_announce_at: instant_to_unix(tracker.next_announce, now),
                    last_success_at: instant_to_unix(tracker.last_success, now),
                    failure_reason: tracker_failure_reason(&tracker.status),
                    warning_message: tracker_warning_message(&tracker.status),
                    seeders: tracker.scrape_complete.map(i64::from),
                    leechers: tracker.scrape_incomplete.map(i64::from),
                    completed: tracker.scrape_downloaded.map(i64::from),
                    uploaded: db_i64(uploaded),
                    downloaded: db_i64(downloaded),
                    left_bytes: left,
                });
                tracker_index += 1;
            }
        }
        let info_hash = self.info_hash_hex.clone();
        self.db
            .run("persist_tracker_state", move |db| {
                rt_db::replace_torrent_trackers(db, &info_hash, &rows)
                    .map_err(|error| error.to_string())
            })
            .await?;
        let mut reg = self.registry.write().await;
        if let Some(mut entry) = reg.get_mut(&self.info_hash_hex) {
            entry.tracker_message = tracker_message;
        };
        Ok(())
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

    async fn apply_file_policy_from_db(&mut self) -> Result<(), String> {
        let info_hash = self.info_hash_hex.clone();
        let (rows, limits) = self
            .db
            .run("load_file_policy", move |db| {
                let rows = rt_db::list_torrent_files(db, &info_hash)
                    .map_err(|error| format!("loading torrent file policy: {error}"))?;
                let limits = Self::torrent_limits_from_db(db, &info_hash)?;
                Ok((rows, limits))
            })
            .await?;
        let piece_count = self.piece_map.piece_count as usize;
        if rows.is_empty() {
            // No per-file projection means the metainfo defaults apply. This
            // also clears a stale policy after the durable projection is
            // intentionally removed.
            self.apply_piece_policy(&vec![true; piece_count], Vec::new());
            return Ok(());
        }
        let file_lengths: HashMap<u32, u64> = self
            .meta
            .files
            .iter()
            .map(|file| (file.index, file.length))
            .collect();
        let mut policy = HashMap::with_capacity(rows.len());
        for row in rows {
            let file_index = u32::try_from(row.file_index).map_err(|_| {
                format!(
                    "persisted file policy has invalid file index {}",
                    row.file_index
                )
            })?;
            if !file_lengths.contains_key(&file_index) {
                return Err(format!(
                    "persisted file policy references unknown file index {file_index}"
                ));
            }
            if !(0..=2).contains(&row.priority) {
                return Err(format!(
                    "persisted file policy has invalid priority {} for file {file_index}",
                    row.priority
                ));
            }
            if policy
                .insert(file_index, (row.wanted, row.priority))
                .is_some()
            {
                return Err(format!(
                    "persisted file policy contains duplicate file index {file_index}"
                ));
            }
        }
        let mut enabled = vec![false; piece_count];
        let mut priority_pieces = Vec::new();
        for piece in 0..self.piece_map.piece_count {
            let regions = self
                .piece_map
                .piece_to_file_regions(piece)
                .map_err(|error| format!("building file policy for piece {piece}: {error}"))?;
            let mut any_wanted = false;
            let mut any_high = false;
            let mut any_first_last = false;
            for region in regions {
                let (wanted, priority) =
                    policy.get(&region.file_index).copied().unwrap_or((true, 1));
                any_wanted |= wanted && priority > 0;
                any_high |= wanted && priority > 1;
                let file_len = file_lengths
                    .get(&region.file_index)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "piece {piece} references unknown file index {}",
                            region.file_index
                        )
                    })?;
                if limits.first_last_piece_prio && wanted && priority > 0 {
                    if region.file_offset > file_len
                        || region.length > file_len.saturating_sub(region.file_offset)
                    {
                        return Err(format!(
                            "piece {piece} has an out-of-range region for file {}",
                            region.file_index
                        ));
                    }
                    any_first_last |= region.file_offset == 0
                        || region.file_offset.saturating_add(region.length) >= file_len;
                }
            }
            enabled[piece as usize] = any_wanted;
            if any_high || any_first_last {
                priority_pieces.push(piece as usize);
            }
        }
        self.apply_piece_policy(&enabled, priority_pieces);
        Ok(())
    }

    fn apply_piece_policy(&mut self, enabled: &[bool], priority: Vec<usize>) {
        for (piece, enabled) in enabled.iter().copied().enumerate() {
            self.picker.set_piece_enabled(piece, enabled);
        }
        self.picker.set_priority(priority);
    }

    async fn apply_torrent_limits_from_db(&mut self) -> Result<(), String> {
        let info_hash = self.info_hash_hex.clone();
        let limits = self
            .db
            .run("load_torrent_limits", move |db| {
                Self::torrent_limits_from_db(db, &info_hash)
            })
            .await?;
        self.apply_torrent_limits(&limits).await
    }

    async fn apply_torrent_limits(&mut self, limits: &EngineTorrentLimits) -> Result<(), String> {
        // Validate and install the complete file policy before changing any
        // other in-memory limit. This keeps a malformed durable file-policy
        // row from producing a partially applied runtime configuration.
        self.apply_file_policy_from_db().await?;
        self.picker.set_sequential(limits.sequential_download);
        if let Some(piece) = limits.sequential_download_from_piece {
            self.picker.set_sequential_from_piece(piece as usize);
        }
        self.super_seeding = limits.super_seeding;
        self.seed_ratio_limit = limits
            .seed_ratio_limit
            .filter(|value| value.is_finite() && *value >= 0.0);
        self.seed_idle_limit = limits
            .seed_idle_limit
            .and_then(|minutes| u64::try_from(minutes).ok())
            .filter(|minutes| *minutes > 0)
            .map(|minutes| Duration::from_secs(minutes.saturating_mul(60)));
        self.torrent_max_peers = limits.max_connections.and_then(|value| {
            usize::try_from(value)
                .ok()
                .filter(|connections| *connections > 0)
        });
        self.set_torrent_download_limit(limits.download_limit);
        self.set_torrent_upload_limit(limits.upload_limit);
        Ok(())
    }

    fn fail_closed_runtime_policy(&mut self, operation: &str, error: &str) {
        for piece in 0..self.piece_map.piece_count {
            self.picker.set_piece_enabled(piece as usize, false);
        }
        self.picker.set_priority(Vec::new());
        self.paused = true;
        warn!(
            component = "torrent",
            operation,
            torrent = %self.info_hash_hex,
            result = "paused",
            error,
            "persisted runtime policy could not be loaded; torrent starts paused with piece selection disabled"
        );
    }

    fn peer_capacity(&self) -> usize {
        self.torrent_max_peers
            .map(|limit| limit.min(self.max_peers))
            .unwrap_or(self.max_peers)
    }

    fn set_torrent_download_limit(&mut self, limit: Option<i64>) {
        self.torrent_download_limit_bytes_per_sec =
            limit.and_then(|value| (value > 0).then_some(value as u64));
        self.recompute_download_limit();
    }

    fn recompute_download_limit(&mut self) {
        // The process-wide limiter is shared by every task. Including the
        // global value here would divide the global allowance once per
        // torrent and make the configured limit unusably strict at scale.
        self.download_limit_bytes_per_sec = self.torrent_download_limit_bytes_per_sec;
        self.download_tokens_updated = Instant::now();
        self.download_tokens = self.download_limit_bytes_per_sec.unwrap_or(u64::MAX);
    }

    fn set_torrent_upload_limit(&mut self, limit: Option<i64>) {
        self.torrent_upload_limit_bytes_per_sec =
            limit.and_then(|value| (value > 0).then_some(value as u64));
        self.recompute_upload_limit();
    }

    fn recompute_upload_limit(&mut self) {
        // See recompute_download_limit: global traffic is enforced by the
        // engine-owned shared bucket, while this field is per torrent.
        self.upload_limit_bytes_per_sec = self.torrent_upload_limit_bytes_per_sec;
        let desired = self.upload_limit_bytes_per_sec;
        let mut queue_full = 0u64;
        for handle in self.active_peers.values_mut() {
            match handle
                .cmd_tx
                .try_send(PeerCommand::UpdateUploadLimit(desired))
            {
                Ok(()) => handle.pending_upload_limit = None,
                Err(_) => {
                    // Keep the latest desired value. A stale queued command
                    // may still be delivered first, but this pending value
                    // will then be retried after it and restores the current
                    // policy instead of silently losing the update.
                    handle.pending_upload_limit = Some(desired);
                    queue_full = queue_full.saturating_add(1);
                }
            }
        }
        self.peer_command_queue_full = self.peer_command_queue_full.saturating_add(queue_full);
    }

    fn torrent_limits_from_db(
        db: &Connection,
        info_hash: &str,
    ) -> Result<EngineTorrentLimits, String> {
        match rt_db::get_torrent_limits(db, info_hash) {
            Ok(row) => Ok(EngineTorrentLimits {
                download_limit: row.download_limit,
                upload_limit: row.upload_limit,
                max_connections: row.max_connections,
                seed_ratio_limit: row.seed_ratio_limit,
                seed_idle_limit: row.seed_idle_limit,
                sequential_download: row.sequential_download,
                sequential_download_from_piece: row.sequential_download_from_piece,
                first_last_piece_prio: row.first_last_piece_prio,
                force_start: row.force_start,
                super_seeding: row.super_seeding,
                auto_tmm: row.auto_tmm,
                auto_management: row.auto_management,
            }),
            Err(rt_db::DbError::NotFound(_)) => Ok(EngineTorrentLimits::default()),
            Err(error) => Err(format!("loading torrent limits: {error}")),
        }
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
                        )
                        .await;
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
                        )
                        .await;
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
                        )
                        .await;
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
                    )
                    .await;
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
            )
            .await;
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
                Ok(TorrentCmd::ReloadFilePolicy { reply }) => {
                    let result = self.apply_file_policy_from_db().await;
                    if let Err(error) = &result {
                        warn!(
                            component = "torrent",
                            operation = "reload_file_policy",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to reload persisted file policy during recheck; retaining the active policy"
                        );
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Ok(TorrentCmd::UpdateLimits { limits, reply }) => {
                    let result = self.apply_torrent_limits(&limits).await;
                    if let Err(error) = &result {
                        warn!(
                            component = "torrent",
                            operation = "update_limits",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to apply persisted torrent limits during recheck"
                        );
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Ok(TorrentCmd::UpdatePeerExchange(enabled)) => {
                    self.pex_enabled = enabled;
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
                Ok(TorrentCmd::BanPeer(peer)) => {
                    self.evict_peer(peer);
                }
                Ok(TorrentCmd::GetPeers { reply }) => {
                    let _ = reply.send(Vec::new());
                }
                Ok(TorrentCmd::GetWebseeds { reply }) => {
                    let _ = reply.send(self.webseed_snapshots());
                }
                Ok(TorrentCmd::GetRuntimeStats { reply }) => {
                    let _ = reply.send(self.runtime_stats());
                }
                Ok(TorrentCmd::AcceptPeer { .. }) => {}
                Ok(TorrentCmd::AcceptUtpPeer { .. }) => {}
                Ok(TorrentCmd::QuiesceForStorageMove { reply }) => {
                    let was_paused = self.paused;
                    self.paused = true;
                    self.shutdown_peers().await;
                    self.announce_stopped().await;
                    let _ = reply.send(was_paused);
                    return Some(RecheckOutcome::Paused);
                }
                Ok(TorrentCmd::ResumeAfterStorageMove {
                    new_save_root,
                    resume_paused,
                }) => {
                    if let Some(new_root) = new_save_root {
                        self.save_root = new_root.clone();
                        self.storage = MountScheduler::new_for_path(
                            StorageRootId::new(),
                            &new_root,
                            &SchedulerConfig {
                                profile: StorageProfile::Unknown,
                                resources: Some(self.resources.clone()),
                                storage_io: self.storage.io_config().clone(),
                                ..Default::default()
                            },
                        );
                        self.prepared_files
                            .lock()
                            .expect("prepared_files mutex poisoned")
                            .clear();
                    }
                    self.paused = resume_paused;
                }
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

        let hints = collect_file_hints(&self.save_root, &self.meta);
        let invalidated = state.apply_file_hints(hints, &self.piece_map);
        if invalidated > 0 {
            warn!(
                torrent = %self.info_hash_hex,
                invalidated,
                "fastresume file hints changed"
            );
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
            if let Some(mut entry) = reg.get_mut(&self.info_hash_hex) {
                entry.stats.uploaded = state.uploaded_bytes;
                entry.stats.downloaded = state.downloaded_bytes;
                entry.total_length = self.meta.total_length();
                entry.amount_left = self.picker.bytes_left();
            };
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
            // Padding files are still written (even though real clients
            // never create them): `PieceVerifier::verify_piece`
            // (rt-storage/src/verify.rs) reads every region composing a
            // piece from disk during recheck and treats a missing file as
            // the whole piece being unverifiable, not as an implicit-zero
            // region. Skipping the write here would make any piece that
            // straddles a padding boundary permanently fail recheck. See
            // also `file.pad` handling in `collect_file_hints`/rt-migrate,
            // which does make padding files optional for fastresume trust.
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
            // Padding files are still written - see write_block.
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

    async fn set_state(&mut self, state: TorrentState) {
        let (previous, row, state_event) = {
            let mut reg = self.registry.write().await;
            let Some(mut entry) = reg.get_mut(&self.info_hash_hex) else {
                return;
            };
            let previous = entry.clone();
            let previous_state = entry.state;
            if let Err(error) = entry.transition(state) {
                warn!(
                    component = "torrent",
                    operation = "transition_state",
                    torrent = %self.info_hash_hex,
                    state = %state,
                    result = "rejected",
                    error = %error,
                    "rejected invalid torrent state transition"
                );
                return;
            }
            entry.total_length = self.meta.total_length();
            entry.amount_left = self.picker.bytes_left();
            if state == TorrentState::Seeding && entry.completed_at.is_none() {
                entry.amount_left = 0;
                entry.completed_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
            let row = crate::engine::row_from_entry(&entry, &TorrentMeta::V1(self.meta.clone()));
            let state_event = (previous_state != state).then(|| rt_db::SessionEventRow {
                event_id: None,
                occurred_at: db_i64(unix_now()),
                info_hash: Some(self.info_hash_hex.clone()),
                kind: "torrent_state_changed".to_owned(),
                message: Some("torrent runtime state changed".to_owned()),
                payload: serde_json::json!({
                    "from": previous_state.as_str(),
                    "to": state.as_str(),
                    "total_length": entry.total_length,
                    "amount_left": entry.amount_left,
                })
                .to_string(),
            });
            (previous, row, state_event)
        };
        let retention = self.event_retention;
        let persistence = self
            .db
            .run("persist_torrent_state", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                if let Some(event) = state_event.as_ref() {
                    rt_db::append_session_event_in_tx(&tx, event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                }
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            if let Some(mut entry) = self.registry.write().await.get_mut(&self.info_hash_hex) {
                *entry = previous;
            }
            warn!(
                component = "db",
                operation = "persist_torrent_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist torrent state"
            );
        } else {
            // The timer is runtime state derived from the durable state. Do
            // not start/stop it before the transition and its database row
            // have both committed: a rejected transition or failed write
            // must leave seed-limit enforcement observing the old state.
            if state == TorrentState::Seeding {
                self.seeding_started_at.get_or_insert_with(Instant::now);
            } else {
                self.seeding_started_at = None;
            }
            self.transfer_stats_dirty = false;
        }
    }

    async fn persist_progress(&mut self) {
        let (previous, row) = {
            let mut reg = self.registry.write().await;
            let Some(mut entry) = reg.get_mut(&self.info_hash_hex) else {
                return;
            };
            let previous = entry.clone();
            entry.total_length = self.meta.total_length();
            entry.amount_left = self.picker.bytes_left();
            let row = crate::engine::row_from_entry(&entry, &TorrentMeta::V1(self.meta.clone()));
            (previous, row)
        };
        let persistence = self
            .db
            .run("persist_torrent_progress", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            if let Some(mut entry) = self.registry.write().await.get_mut(&self.info_hash_hex) {
                *entry = previous;
            }
            warn!(
                component = "db",
                operation = "persist_torrent_progress",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist torrent progress"
            );
        } else {
            self.transfer_stats_dirty = false;
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

    async fn persist_recheck_job_progress(
        &self,
        job_id: &str,
        next_piece: u32,
        valid_pieces: usize,
        invalid_pieces: &[i64],
        state: &str,
        message: Option<&str>,
    ) {
        let now = unix_now();
        let done = i64::from(next_piece.min(self.piece_map.piece_count));
        let byte_offset = self.verified_byte_offset(next_piece);
        let verified_bytes = db_i64((valid_pieces as u64).saturating_mul(self.meta.piece_length));
        let job_id = job_id.to_owned();
        let state = state.to_owned();
        let message = message.map(str::to_owned);
        let invalid_pieces = invalid_pieces.to_vec();
        let log_job_id = job_id.clone();
        let log_state = state.clone();
        let persistence = self
            .db
            .run("persist_recheck_progress", move |db| {
                let mut job = rt_db::get_job(db, &job_id).map_err(|error| error.to_string())?;
                job.state = state.clone();
                job.done = done;
                job.checkpoint = done;
                job.piece_index = Some(done);
                job.byte_offset = Some(byte_offset);
                job.verified_bytes = verified_bytes;
                job.invalid_pieces = invalid_pieces.clone();
                job.updated_at = db_i64(now);
                if matches!(state.as_str(), JOB_STATE_CANCELLED | JOB_STATE_COMPLETED) {
                    job.finished_at = Some(db_i64(now));
                }
                let event = rt_db::JobEventRow {
                    event_id: None,
                    job_id: job_id.clone(),
                    occurred_at: db_i64(now),
                    kind: match state.as_str() {
                        JOB_STATE_CANCELLED => "check_cancelled",
                        JOB_STATE_COMPLETED => "check_completed",
                        _ => "check_progress",
                    }
                    .to_owned(),
                    message,
                    payload: serde_json::json!({
                        "piece_index": job.piece_index,
                        "verified_bytes": job.verified_bytes,
                        "invalid_pieces": job.invalid_pieces,
                        "state": state,
                    })
                    .to_string(),
                };
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
                rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(e) = persistence {
            warn!(
                component = "db",
                operation = "persist_recheck_progress_and_event",
                job_id = %log_job_id,
                state = %log_state,
                result = "error",
                error = %e,
                "failed to persist recheck progress and event atomically"
            );
        }
    }

    fn verified_byte_offset(&self, next_piece: u32) -> i64 {
        let bytes = (next_piece as u64).saturating_mul(self.meta.piece_length);
        db_i64(bytes.min(self.meta.total_length()))
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
        state.file_hints = collect_file_hints(&self.save_root, &self.meta);
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

fn reset_webseed_sleep(sleep: &mut Pin<Box<Sleep>>, delay: Duration) {
    sleep.as_mut().reset(tokio::time::Instant::now() + delay);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutgoingTransportPolicy {
    Auto,
    TcpOnly,
    PreferUtp,
    UtpOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerSource {
    Tracker,
    Dht,
    PeerExchange,
    Manual,
}

fn outgoing_transport_policy_configured() -> OutgoingTransportPolicy {
    if let Ok(value) = std::env::var("TNG_UTP_OUTGOING") {
        return parse_outgoing_transport_policy(&value);
    }
    if std::env::var_os("TNG_ENABLE_UTP_OUTGOING").is_some() {
        return OutgoingTransportPolicy::PreferUtp;
    }
    OutgoingTransportPolicy::Auto
}

fn parse_outgoing_transport_policy(value: &str) -> OutgoingTransportPolicy {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "default" => OutgoingTransportPolicy::Auto,
        "0" | "false" | "no" | "off" | "tcp" | "tcp-only" => OutgoingTransportPolicy::TcpOnly,
        "1" | "true" | "yes" | "prefer" | "prefer-utp" | "utp-prefer" => {
            OutgoingTransportPolicy::PreferUtp
        }
        "only" | "utp" | "utp-only" => OutgoingTransportPolicy::UtpOnly,
        _ => OutgoingTransportPolicy::Auto,
    }
}

fn outgoing_transport_policy_for_peer(
    configured: OutgoingTransportPolicy,
    source: PeerSource,
    private: bool,
) -> OutgoingTransportPolicy {
    if private {
        return OutgoingTransportPolicy::TcpOnly;
    }
    match configured {
        OutgoingTransportPolicy::Auto => match source {
            PeerSource::Tracker => OutgoingTransportPolicy::TcpOnly,
            PeerSource::Dht | PeerSource::PeerExchange | PeerSource::Manual => {
                OutgoingTransportPolicy::PreferUtp
            }
        },
        explicit => explicit,
    }
}

/// Collect on-disk size/mtime/inode hints for every file in `meta`.
///
/// Per-file, not all-or-nothing: a file that can't be stat'd (missing,
/// permission error, a BEP47 padding file real clients never materialize,
/// ...) is simply omitted from the result rather than aborting the whole
/// collection. `apply_file_hints` already treats a missing hint as "this
/// file changed" and invalidates only *that* file's pieces — one bad file
/// must not poison fastresume trust for every other file in the torrent.
fn collect_file_hints(root: &std::path::Path, meta: &TorrentMetaV1) -> Vec<FileHint> {
    meta.files
        .iter()
        .filter_map(|file| {
            let path = file.path.resolve(root);
            let metadata = match rt_storage::metadata_no_follow(&path) {
                Ok(metadata) => metadata,
                Err(e) => {
                    debug!(
                        component = "fastresume",
                        operation = "collect_file_hints",
                        file_index = file.index,
                        path = %path.display(),
                        error = %e,
                        "could not stat file; omitting its hint"
                    );
                    return None;
                }
            };
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

            Some(FileHint {
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

/// Restore the small amount of tracker session state that must survive an
/// actor restart. Tracker IDs are opaque BEP 3 bytes and cannot be recovered
/// from the metainfo; losing them makes the next HTTP announce a new tracker
/// session. A persisted next deadline is restored as well so startup does
/// not create an announce storm for every resumed torrent. The query itself
/// is issued through `DbExecutor` before this pure projection runs.
fn restore_tracker_state_from_rows(
    tiers: &mut [Vec<TrackerState>],
    rows: &[rt_db::TorrentTrackerRow],
) {
    let persisted = rows
        .iter()
        .map(|row| (row.url.clone(), row))
        .collect::<HashMap<_, _>>();
    let now = Instant::now();
    let now_unix = db_i64(unix_now());
    for tier in tiers {
        for tracker in tier {
            let Some(row) = persisted.get(&tracker.url) else {
                continue;
            };
            tracker.tracker_id = row.tracker_id.clone();
            tracker.next_announce = row.next_announce_at.map(|deadline| {
                let delay = deadline.saturating_sub(now_unix).max(0) as u64;
                now + Duration::from_secs(delay)
            });
        }
    }
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
        IpAddr::V6(ip) => {
            ip.is_loopback() || is_ipv6_unique_local(ip) || is_ipv6_unicast_link_local(ip)
        }
    }
}

fn is_ipv6_unique_local(ip: std::net::Ipv6Addr) -> bool {
    ip.segments()[0] & 0xfe00 == 0xfc00
}

fn is_ipv6_unicast_link_local(ip: std::net::Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfe80
}

fn record_peer_transfer(peer: &mut PeerHandle, upload: bool, bytes: u64) {
    let now = Instant::now();
    if upload {
        peer.uploaded = peer.uploaded.saturating_add(bytes);
        peer.upload_rate_window = peer.upload_rate_window.saturating_add(bytes);
        let elapsed = now.saturating_duration_since(peer.upload_rate_window_started);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        peer.upload_rate = peer.upload_rate_window as f64 / elapsed.as_secs_f64().max(0.001);
        peer.upload_rate_window = 0;
        peer.upload_rate_window_started = now;
    } else {
        peer.downloaded = peer.downloaded.saturating_add(bytes);
        peer.download_rate_window = peer.download_rate_window.saturating_add(bytes);
        let elapsed = now.saturating_duration_since(peer.download_rate_window_started);
        if elapsed < Duration::from_secs(1) {
            return;
        }
        peer.download_rate = peer.download_rate_window as f64 / elapsed.as_secs_f64().max(0.001);
        peer.download_rate_window = 0;
        peer.download_rate_window_started = now;
    }
}

fn peer_rate(rate: f64, last_sample: Instant) -> i64 {
    if Instant::now().saturating_duration_since(last_sample) > Duration::from_secs(15) {
        0
    } else {
        rate.max(0.0).round() as i64
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

#[derive(Debug, Default, PartialEq, Eq)]
struct UtPexPeers {
    added: Vec<SocketAddr>,
    dropped: Vec<SocketAddr>,
}

/// Parses BEP 11 compact IPv4/IPv6 `added`, `added6`, `dropped`, and
/// `dropped6` lists. The dropped set remains advisory at the task boundary:
/// it removes stale retry candidates but never forcibly disconnects an
/// otherwise healthy local connection.
fn parse_ut_pex_peers(payload: &[u8]) -> anyhow::Result<UtPexPeers> {
    let value = decode(payload)?;
    let BValue::Dict(pairs) = value else {
        anyhow::bail!("ut_pex payload must be a dict");
    };
    let parse_field = |key: &[u8], width: usize| -> anyhow::Result<Vec<SocketAddr>> {
        let Some(bytes) = pairs
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .and_then(|(_, value)| value.as_bytes())
        else {
            return Ok(Vec::new());
        };
        if bytes.len() % width != 0 {
            anyhow::bail!("ut_pex {key:?} peers length is not a multiple of {width}");
        }
        let count = bytes.len() / width;
        if count > MAX_UT_PEX_PEERS {
            anyhow::bail!("ut_pex {key:?} peer list exceeds {MAX_UT_PEX_PEERS} entries");
        }
        let mut peers = Vec::with_capacity(count);
        if width == 6 {
            for chunk in bytes.as_chunks::<6>().0 {
                let port = u16::from_be_bytes([chunk[4], chunk[5]]);
                if port != 0 {
                    peers.push(SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]),
                        port,
                    )));
                }
            }
        } else {
            for chunk in bytes.as_chunks::<18>().0 {
                let port = u16::from_be_bytes([chunk[16], chunk[17]]);
                if port != 0 {
                    let octets: [u8; 16] = chunk[0..16].try_into().expect("chunk is 18 bytes");
                    peers.push(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::from(octets),
                        port,
                        0,
                        0,
                    )));
                }
            }
        }
        Ok(peers)
    };
    let added = parse_field(b"added", 6)?;
    let added6 = parse_field(b"added6", 18)?;
    let dropped = parse_field(b"dropped", 6)?;
    let dropped6 = parse_field(b"dropped6", 18)?;
    if added.len() + added6.len() + dropped.len() + dropped6.len() > MAX_UT_PEX_PEERS {
        anyhow::bail!("ut_pex peer list exceeds {MAX_UT_PEX_PEERS} entries");
    }
    Ok(UtPexPeers {
        added: added.into_iter().chain(added6).collect(),
        dropped: dropped.into_iter().chain(dropped6).collect(),
    })
}

fn reconcile_peer_availability<A: PieceAvailability + ?Sized>(
    availability: &mut Availability,
    old: &A,
    new: &[bool],
) {
    let piece_count = availability.piece_count();
    for piece in 0..piece_count {
        match (
            old.has_piece(piece),
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
    let current = db_i64(unix_now());
    instant.map(|instant| {
        if instant >= now {
            current.saturating_add(db_i64(instant.duration_since(now).as_secs()))
        } else {
            current.saturating_sub(db_i64(now.duration_since(instant).as_secs()))
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

async fn send_peer_event(peer_event_tx: &mpsc::Sender<PeerEvent>, event: PeerEvent) -> bool {
    matches!(
        timeout(PEER_EVENT_SEND_TIMEOUT, peer_event_tx.send(event)).await,
        Ok(Ok(()))
    )
}

/// Open a TCP connection, complete BEP 3 handshake, receive Piece messages.
async fn run_outgoing_peer_with_policy(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    policy: OutgoingTransportPolicy,
) -> anyhow::Result<()> {
    match policy {
        OutgoingTransportPolicy::Auto => {
            run_outgoing_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload).await
        }
        OutgoingTransportPolicy::TcpOnly => {
            run_outgoing_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload).await
        }
        OutgoingTransportPolicy::UtpOnly => {
            run_outgoing_utp_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload).await
        }
        OutgoingTransportPolicy::PreferUtp => match UtpStream::connect(addr).await {
            Ok(stream) => {
                run_established_utp_peer(
                    addr,
                    info_hash,
                    peer_event_tx,
                    peer_cmd_rx,
                    upload,
                    stream,
                )
                .await
            }
            Err(e) => {
                debug!(
                    component = "peer",
                    operation = "connect_utp",
                    peer = %addr,
                    result = "fallback",
                    error = %e,
                    "uTP peer path failed; falling back to TCP"
                );
                run_outgoing_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload).await
            }
        },
    }
}

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
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    // Send our handshake as raw bytes before the codec takes over.
    {
        use tokio::io::AsyncWriteExt;
        let inner = framed.get_mut();
        timeout(PEER_SOCKET_WRITE_TIMEOUT, inner.write_all(&our_hs.encode()))
            .await
            .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;
    }

    let remote_supports_extension = {
        use tokio::io::AsyncReadExt;
        let mut hs_buf = [0u8; 68];
        tokio::time::timeout(
            Duration::from_secs(10),
            framed.get_mut().read_exact(&mut hs_buf),
        )
        .await??;
        let remote_hs = Handshake::parse(&hs_buf)?;
        if remote_hs.info_hash != info_hash {
            anyhow::bail!("info_hash mismatch from {addr}");
        }
        if remote_hs.peer_id == crate::peer_id::our_peer_id() {
            anyhow::bail!("peer handshake identifies this client");
        }
        remote_hs.reserved.supports_extension_protocol()
    };

    let mut peer_io = PeerIo::Tcp(framed);
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut peer_io, &upload.have_pieces).await?;
    peer_io.send(Message::Interested).await?;

    run_peer_loop(addr, peer_io, peer_event_tx, peer_cmd_rx, upload).await
}

async fn run_outgoing_utp_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
) -> anyhow::Result<()> {
    let stream = UtpStream::connect(addr).await?;
    run_established_utp_peer(addr, info_hash, peer_event_tx, peer_cmd_rx, upload, stream).await
}

async fn run_established_utp_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    mut stream: UtpStream,
) -> anyhow::Result<()> {
    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    timeout(
        PEER_SOCKET_WRITE_TIMEOUT,
        stream.write_all(&our_hs.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;

    let mut hs_buf = [0u8; 68];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut hs_buf)).await??;
    let remote_hs = Handshake::parse(&hs_buf)?;
    if remote_hs.info_hash != info_hash {
        anyhow::bail!("info_hash mismatch from {addr}");
    }
    if remote_hs.peer_id == crate::peer_id::our_peer_id() {
        anyhow::bail!("peer handshake identifies this client");
    }
    let remote_supports_extension = remote_hs.reserved.supports_extension_protocol();

    let mut peer_io = PeerIo::Utp(UtpPeerIo { stream });
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut peer_io, &upload.have_pieces).await?;
    peer_io.send(Message::Interested).await?;

    run_peer_loop(addr, peer_io, peer_event_tx, peer_cmd_rx, upload).await
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
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    {
        use tokio::io::AsyncWriteExt;
        timeout(
            PEER_SOCKET_WRITE_TIMEOUT,
            framed.get_mut().write_all(&our_hs.encode()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;
    }

    let mut peer_io = PeerIo::Tcp(framed);
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut peer_io, &upload.have_pieces).await?;
    peer_io.send(Message::Interested).await?;
    run_peer_loop(addr, peer_io, peer_event_tx, peer_cmd_rx, upload).await
}

async fn run_incoming_utp_peer(
    mut stream: UtpStream,
    addr: SocketAddr,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    remote_supports_extension: bool,
) -> anyhow::Result<()> {
    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    timeout(
        PEER_SOCKET_WRITE_TIMEOUT,
        stream.write_all(&our_hs.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;

    let mut peer_io = PeerIo::Utp(UtpPeerIo { stream });
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(&mut peer_io, &upload.have_pieces).await?;
    peer_io.send(Message::Interested).await?;
    run_peer_loop(addr, peer_io, peer_event_tx, peer_cmd_rx, upload).await
}

enum PeerIo {
    Tcp(Framed<TcpStream, PeerCodec>),
    Utp(UtpPeerIo),
}

struct UtpPeerIo {
    stream: UtpStream,
}

impl PeerIo {
    async fn send(&mut self, msg: Message) -> anyhow::Result<()> {
        timeout(PEER_SOCKET_WRITE_TIMEOUT, async {
            match self {
                PeerIo::Tcp(framed) => framed.send(msg).await.map_err(Into::into),
                PeerIo::Utp(io) => io.send(msg).await,
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("peer socket write timed out"))?
    }

    async fn next(&mut self) -> anyhow::Result<Option<Message>> {
        match self {
            PeerIo::Tcp(framed) => match framed.next().await {
                Some(result) => result.map(Some).map_err(Into::into),
                None => Ok(None),
            },
            PeerIo::Utp(io) => io.next().await,
        }
    }
}

impl UtpPeerIo {
    async fn send(&mut self, msg: Message) -> anyhow::Result<()> {
        self.stream.write_all(&msg.encode()).await?;
        Ok(())
    }

    async fn next(&mut self) -> anyhow::Result<Option<Message>> {
        let mut len_buf = [0u8; 4];
        match self.stream.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(rt_utp::UtpError::Closed) => return Ok(None),
            Err(err) => return Err(err.into()),
        }
        let len = u32::from_be_bytes(len_buf);
        if len > rt_peer_wire::message::MAX_MESSAGE_LEN {
            anyhow::bail!("peer message too large: {len}");
        }
        let mut payload = vec![0u8; len as usize];
        if len > 0 {
            self.stream.read_exact(&mut payload).await?;
        }
        Ok(Some(Message::parse(&payload)?))
    }
}

async fn run_peer_loop(
    addr: SocketAddr,
    mut peer_io: PeerIo,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    mut peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    mut upload: UploadContext,
) -> anyhow::Result<()> {
    let mut outstanding = Vec::<OutstandingRequest>::new();
    let mut upload_choked = true;
    let mut upload_limit_bytes_per_sec = upload.upload_limit_bytes_per_sec;
    let mut upload_tokens = upload_limit_bytes_per_sec.unwrap_or(u64::MAX);
    let mut upload_tokens_updated = Instant::now();
    let mut timeout_tick = interval(Duration::from_secs(5));
    let mut last_activity = Instant::now();
    let mut request_window_started = Instant::now();
    let mut upload_requests_in_window = 0u32;
    // Disk reads are scheduled through the bounded mount scheduler, but the
    // peer loop must not await one read inline: a slow device otherwise
    // prevents it from processing choke/shutdown/input messages. The loop
    // owns the socket; detached read futures only return a leased block.
    let mut upload_reads = futures::stream::FuturesUnordered::new();
    // A peer is allowed to pipeline more requests than the detached read
    // budget. Queue only a small bounded tail and let completed reads refill
    // the worker set; saturating this queue drops the newest request instead
    // of killing an otherwise valid connection.
    let mut pending_upload_requests = VecDeque::with_capacity(MAX_QUEUED_UPLOAD_REQUESTS);
    let mut upload_request_drops = 0u64;

    let result: anyhow::Result<()> = async {
        loop {
        tokio::select! {
            Some(cmd) = peer_cmd_rx.recv() => {
                last_activity = Instant::now();
                match cmd {
                    PeerCommand::Request(req) => {
                        peer_io.send(Message::Request {
                            piece: req.piece,
                            begin: req.begin,
                            length: req.length,
                        }).await?;
                        outstanding.push(OutstandingRequest::new(req));
                    }
                    PeerCommand::Have(piece) => {
                        upload.have_pieces.set(piece as usize, true);
                        peer_io.send(Message::Have(piece)).await?;
                    }
                    PeerCommand::Choke => {
                        upload_choked = true;
                        pending_upload_requests.clear();
                        peer_io.send(Message::Choke).await?;
                    }
                    PeerCommand::Unchoke => {
                        upload_choked = false;
                        peer_io.send(Message::Unchoke).await?;
                        start_upload_reads(
                            &mut upload_reads,
                            &mut pending_upload_requests,
                            &upload,
                            upload_choked,
                        );
                    }
                    PeerCommand::UpdateUploadLimit(limit) => {
                        upload_limit_bytes_per_sec = limit;
                        upload_tokens = upload_limit_bytes_per_sec.unwrap_or(u64::MAX);
                        upload_tokens_updated = Instant::now();
                    }
                    PeerCommand::Shutdown => {
                        pending_upload_requests.clear();
                        break;
                    }
                }
            }
            msg_result = peer_io.next() => {
                let Some(msg) = msg_result? else {
                    break;
                };
                last_activity = Instant::now();
                match msg {
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
                        if !send_peer_event(&peer_event_tx, PeerEvent::Bitfield { peer: addr, pieces }).await {
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
                        if !send_peer_event(&peer_event_tx, PeerEvent::Have { peer: addr, piece }).await {
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
                        // Charge the aggregate budget for the payload that
                        // actually arrived, not for a request that might
                        // never have been fulfilled.
                        upload.global_download.acquire(data_len as u64).await;
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Piece {
                                peer: addr,
                                block: BlockEvent {
                                    piece,
                                    offset: begin,
                                    data: bytes::Bytes::from(data),
                                },
                            },
                        )
                        .await
                        {
                            break; // torrent task gone
                        }
                    }
                    Message::Unchoke => {
                        if !send_peer_event(&peer_event_tx, PeerEvent::Unchoked { peer: addr }).await {
                            break;
                        }
                    }
                    Message::Interested => {
                        if !send_peer_event(&peer_event_tx, PeerEvent::Interested { peer: addr }).await {
                            break;
                        }
                    }
                    Message::NotInterested => {
                        if !send_peer_event(&peer_event_tx, PeerEvent::NotInterested { peer: addr }).await {
                            break;
                        }
                    }
                    Message::Request { piece, begin, length } => {
                        let now = Instant::now();
                        if now.duration_since(request_window_started) >= PEER_UPLOAD_REQUEST_WINDOW {
                            request_window_started = now;
                            upload_requests_in_window = 0;
                        }
                        upload_requests_in_window = upload_requests_in_window.saturating_add(1);
                        if upload_requests_in_window > MAX_PEER_UPLOAD_REQUESTS_PER_WINDOW {
                            upload_request_drops = upload_request_drops.saturating_add(1);
                            if upload_request_drops == 1 || upload_request_drops.is_multiple_of(256)
                            {
                                debug!(
                                    component = "peer",
                                    operation = "queue_upload_request",
                                    peer = %addr,
                                    result = "dropped",
                                    reason = "request_rate_limit",
                                    dropped = upload_request_drops,
                                    "dropping excess upload requests without terminating peer"
                                );
                            }
                            continue;
                        }
                        if upload_choked || !upload.have_pieces.get(piece as usize).unwrap_or(false) {
                            continue;
                        }
                        if pending_upload_requests.len() >= MAX_QUEUED_UPLOAD_REQUESTS
                            && upload_reads.len() >= MAX_PENDING_UPLOAD_READS
                        {
                            upload_request_drops = upload_request_drops.saturating_add(1);
                            if upload_request_drops == 1 || upload_request_drops.is_multiple_of(256)
                            {
                                debug!(
                                    component = "peer",
                                    operation = "queue_upload_request",
                                    peer = %addr,
                                    result = "dropped",
                                    reason = "upload_queue_saturated",
                                    dropped = upload_request_drops,
                                    "dropping excess upload requests without terminating peer"
                                );
                            }
                            continue;
                        }
                        pending_upload_requests.push_back(UploadRequest {
                            piece,
                            begin,
                            length,
                        });
                        start_upload_reads(
                            &mut upload_reads,
                            &mut pending_upload_requests,
                            &upload,
                            upload_choked,
                        );
                    }
                    Message::Choke => {
                        let choked_requests = drain_outstanding(&mut outstanding);
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Choked {
                                peer: addr,
                                outstanding: choked_requests,
                            },
                        )
                        .await
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
                                if !send_peer_event(
                                    &peer_event_tx,
                                    PeerEvent::ExtendedHandshake {
                                        peer: addr,
                                        ut_metadata_id: handshake.ut_metadata_id(),
                                        ut_pex_id: handshake.ut_pex_id(),
                                        metadata_size: handshake.metadata_size,
                                    },
                                )
                                .await
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
                                peer_io.send(Message::Extended {
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
                        if !upload.pex_enabled || upload.is_private {
                            continue;
                        }
                        match parse_ut_pex_peers(&payload) {
                            Ok(pex) if !pex.added.is_empty() || !pex.dropped.is_empty() => {
                                if !send_peer_event(
                                    &peer_event_tx,
                                    PeerEvent::PeerExchange {
                                        peer: addr,
                                        peers: pex.added,
                                        dropped: pex.dropped,
                                    },
                                )
                                .await
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
            upload_result = upload_reads.next(), if !upload_reads.is_empty() => {
                let Some(upload_result) = upload_result else {
                    continue;
                };
                let (request, result) = match upload_result {
                    Ok(result) => result,
                    Err(error) => {
                        warn!(
                            component = "peer",
                            operation = "read_upload_block",
                            peer = %addr,
                            result = "worker_join_error",
                            error = %error,
                            "upload block worker failed"
                        );
                        start_upload_reads(
                            &mut upload_reads,
                            &mut pending_upload_requests,
                            &upload,
                            upload_choked,
                        );
                        continue;
                    }
                };
                match result {
                    Ok(block) if !upload_choked => {
                        let bytes = block.data.len() as u64;
                        wait_for_upload_budget(
                            upload_limit_bytes_per_sec,
                            &mut upload_tokens,
                            &mut upload_tokens_updated,
                            bytes,
                        )
                        .await;
                        upload.global_upload.acquire(bytes).await;
                        peer_io
                            .send(Message::Piece {
                                piece: request.piece,
                                begin: request.begin,
                                data: block.data.to_vec(),
                            })
                            .await?;
                        if !send_peer_event(&peer_event_tx, PeerEvent::Uploaded { peer: addr, bytes }).await {
                            break;
                        }
                    }
                    Ok(_) => {
                        // A local choke or shutdown may have arrived while
                        // the read was in flight. Dropping the leased block
                        // is safer than sending data after the state change.
                    }
                    Err(error) => {
                        warn!(
                            component = "peer",
                            operation = "read_upload_block",
                            peer = %addr,
                            piece = request.piece,
                            begin = request.begin,
                            length = request.length,
                            result = "error",
                            error = %error,
                            "failed to read upload block"
                        );
                    }
                }
                start_upload_reads(
                    &mut upload_reads,
                    &mut pending_upload_requests,
                    &upload,
                    upload_choked,
                );
            }
            _ = timeout_tick.tick() => {
                if last_activity.elapsed() > PEER_IDLE_TIMEOUT {
                    anyhow::bail!("peer idle timeout");
                }
                let timed_out = take_timed_out_requests(&mut outstanding, Duration::from_secs(60));
                if !timed_out.is_empty()
                    && !send_peer_event(
                        &peer_event_tx,
                        PeerEvent::RequestTimedOut {
                            peer: addr,
                            timed_out,
                        },
                    )
                    .await
                {
                    break;
                }
            }
        }
        }
        for read in upload_reads {
            read.abort();
        }
        Ok(())
    }
    .await;

    let _ = send_peer_event(
        &peer_event_tx,
        PeerEvent::Disconnected {
            peer: addr,
            outstanding: drain_outstanding(&mut outstanding),
        },
    )
    .await;
    result
}

async fn send_extension_handshake(
    peer_io: &mut PeerIo,
    metadata: Option<&Arc<Vec<u8>>>,
    is_private: bool,
    pex_enabled: bool,
    remote_supports_extension: bool,
) -> anyhow::Result<()> {
    if remote_supports_extension {
        let handshake = extension_handshake_for_torrent(metadata, is_private, pex_enabled);
        peer_io
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
    pex_enabled: bool,
) -> ExtensionHandshake {
    let metadata_size = metadata.and_then(|bytes| u32::try_from(bytes.len()).ok());
    let mut handshake = ExtensionHandshake::new(metadata_size);
    if metadata_size.is_some() {
        handshake = handshake.with_ut_metadata(LOCAL_UT_METADATA_ID);
    }
    if pex_enabled && !is_private {
        handshake = handshake.with_ut_pex(LOCAL_UT_PEX_ID);
    }
    handshake
}

fn metadata_response(piece: u32, metadata: &[u8]) -> UtMetadataMessage {
    let Some(start) = (piece as usize).checked_mul(METADATA_PIECE_SIZE) else {
        return UtMetadataMessage::Reject { piece };
    };
    if start >= metadata.len() {
        return UtMetadataMessage::Reject { piece };
    }
    let Some(end) = start
        .checked_add(METADATA_PIECE_SIZE)
        .map(|end| end.min(metadata.len()))
    else {
        return UtMetadataMessage::Reject { piece };
    };
    let Ok(total_size) = u32::try_from(metadata.len()) else {
        return UtMetadataMessage::Reject { piece };
    };
    UtMetadataMessage::Data {
        piece,
        total_size,
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

async fn send_have_state(peer_io: &mut PeerIo, have_pieces: &PieceBitmap) -> anyhow::Result<()> {
    let bitfield = have_pieces.to_bitfield();
    if bitfield.iter().any(|byte| *byte != 0) {
        peer_io.send(Message::Bitfield(bitfield)).await?;
    }
    Ok(())
}

async fn read_upload_block(
    upload: &UploadContext,
    piece: u32,
    begin: u32,
    length: u32,
) -> anyhow::Result<LeasedUploadBlock> {
    if length == 0 || length > MAX_BLOCK_SIZE {
        anyhow::bail!("invalid upload block length {length}");
    }
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

fn start_upload_reads(
    upload_reads: &mut futures::stream::FuturesUnordered<tokio::task::JoinHandle<UploadReadResult>>,
    pending_upload_requests: &mut VecDeque<UploadRequest>,
    upload: &UploadContext,
    upload_choked: bool,
) {
    if upload_choked {
        return;
    }
    while upload_reads.len() < MAX_PENDING_UPLOAD_READS {
        let Some(request) = pending_upload_requests.pop_front() else {
            break;
        };
        if !upload
            .have_pieces
            .get(request.piece as usize)
            .unwrap_or(false)
        {
            continue;
        }
        let upload_for_read = upload.clone();
        upload_reads.push(tokio::spawn(async move {
            let result = match timeout(
                PEER_UPLOAD_READ_TIMEOUT,
                read_upload_block(
                    &upload_for_read,
                    request.piece,
                    request.begin,
                    request.length,
                ),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "upload block read exceeded its peer deadline ({}s)",
                    PEER_UPLOAD_READ_TIMEOUT.as_secs()
                )),
            };
            (request, result)
        }));
    }
}

async fn wait_for_upload_budget(
    limit: Option<u64>,
    tokens: &mut u64,
    tokens_updated: &mut Instant,
    bytes: u64,
) {
    let Some(limit) = limit.filter(|limit| *limit > 0) else {
        *tokens = u64::MAX;
        *tokens_updated = Instant::now();
        return;
    };
    loop {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(*tokens_updated);
        *tokens_updated = now;
        let refill = (elapsed.as_secs_f64() * limit as f64).floor() as u64;
        *tokens = tokens.saturating_add(refill).min(limit);
        if *tokens >= bytes {
            *tokens = tokens.saturating_sub(bytes);
            return;
        }
        let missing = bytes.saturating_sub(*tokens);
        sleep(Duration::from_secs_f64(missing as f64 / limit as f64)).await;
    }
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
    if !piece_count.is_multiple_of(8) && !bits.is_empty() {
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

#[cfg(test)]
fn pieces_to_bitfield(pieces: &[bool]) -> Vec<u8> {
    let mut bits = vec![0u8; pieces.len().div_ceil(8)];
    for (idx, has_piece) in pieces.iter().copied().enumerate() {
        if has_piece {
            bits[idx / 8] |= 0x80 >> (idx % 8);
        }
    }
    bits
}

fn super_seed_visible_pieces(have_pieces: &[bool], peer_addr: SocketAddr) -> Vec<bool> {
    let mut visible = vec![false; have_pieces.len()];
    let available: Vec<usize> = have_pieces
        .iter()
        .enumerate()
        .filter_map(|(idx, have)| have.then_some(idx))
        .collect();
    if available.is_empty() {
        return visible;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    peer_addr.hash(&mut hasher);
    let selected = available[(hasher.finish() as usize) % available.len()];
    visible[selected] = true;
    visible
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

fn webseed_retry_delay(failures: u8) -> Duration {
    let shift = failures.saturating_sub(1).min(9) as u32;
    let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
    WEBSEED_RETRY_BASE
        .checked_mul(multiplier)
        .unwrap_or(WEBSEED_RETRY_MAX)
        .min(WEBSEED_RETRY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choke_state_does_not_commit_when_peer_mailbox_is_full() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(PeerCommand::Shutdown)
            .expect("fill the test peer mailbox");

        let (state, failed) =
            TorrentTask::try_apply_choke_decision(&tx, false, ChokeDecision::Choke);
        assert!(!state, "failed delivery must retain the previous state");
        assert!(failed);

        let _ = rx.try_recv();
        let (state, failed) =
            TorrentTask::try_apply_choke_decision(&tx, false, ChokeDecision::Choke);
        assert!(state);
        assert!(!failed);
    }

    #[test]
    fn have_delivery_keeps_a_bounded_retry_until_mailbox_accepts_it() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(PeerCommand::Shutdown)
            .expect("fill the test peer mailbox");
        let mut pending = PieceBitmap::new(8);

        assert!(!TorrentTask::try_send_have(&tx, &mut pending, 3));
        assert_eq!(pending.first_set_u32(), Some(3));

        let _ = rx.try_recv();
        assert!(TorrentTask::try_send_have(&tx, &mut pending, 3));
        assert_eq!(pending.first_set_u32(), None);
        assert!(matches!(rx.try_recv(), Ok(PeerCommand::Have(3))));
    }

    #[test]
    fn webseed_retry_delay_is_exponential_and_bounded() {
        assert_eq!(webseed_retry_delay(1), Duration::from_secs(1));
        assert_eq!(webseed_retry_delay(2), Duration::from_secs(2));
        assert_eq!(webseed_retry_delay(9), Duration::from_secs(256));
        assert_eq!(webseed_retry_delay(10), WEBSEED_RETRY_MAX);
        assert_eq!(webseed_retry_delay(u8::MAX), WEBSEED_RETRY_MAX);
    }

    #[tokio::test]
    async fn transfer_stats_are_batched_until_progress_flush() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 4,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 4;
        entry.transition(TorrentState::Downloading).unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            Arc::clone(&registry),
            DbExecutor::direct(Arc::clone(&db)),
            ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            cmd_rx,
            temp.path().join("fastresume"),
            8,
            6881,
            10,
            10,
            60,
            1024 * 1024,
            StorageIoConfig::default(),
            false,
            OutboundEgressPolicy::default(),
            GlobalNetworkBudget::unlimited(),
            10_000,
        )
        .await;

        task.update_transfer(4, false).await;
        task.update_transfer(2, true).await;
        assert!(task.transfer_stats_dirty);
        assert!(rt_db::get(&db.lock().unwrap(), &info_hash).is_err());

        task.persist_progress().await;

        assert!(!task.transfer_stats_dirty);
        let row = rt_db::get(&db.lock().unwrap(), &info_hash).unwrap();
        assert_eq!(row.uploaded, 2);
        assert_eq!(row.downloaded, 4);
    }

    #[tokio::test]
    async fn seed_ratio_limit_pauses_a_completed_torrent() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [3; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 4,
            pieces: vec![[4; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 4;
        entry.transition(TorrentState::Downloading).unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            Arc::clone(&registry),
            DbExecutor::direct(Arc::clone(&db)),
            ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            cmd_rx,
            temp.path().join("fastresume"),
            8,
            6881,
            10,
            10,
            60,
            1024 * 1024,
            StorageIoConfig::default(),
            false,
            OutboundEgressPolicy::default(),
            GlobalNetworkBudget::unlimited(),
            10_000,
        )
        .await;

        task.picker.mark_have(0);
        task.seed_ratio_limit = Some(0.5);
        task.update_transfer(4, false).await;
        task.update_transfer(2, true).await;
        task.set_state(TorrentState::Seeding).await;
        let events = rt_db::list_session_events(&db.lock().unwrap(), Some(&info_hash), 10).unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "torrent_state_changed"
                && event.payload.contains("downloading")
                && event.payload.contains("seeding")
        }));
        task.enforce_seed_limits().await;

        assert!(task.paused);
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Paused
        );
    }

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
    fn stricter_limit_uses_lowest_enabled_limit() {
        assert_eq!(stricter_limit(Some(10), Some(20)), Some(10));
        assert_eq!(stricter_limit(Some(30), Some(20)), Some(20));
        assert_eq!(stricter_limit(Some(30), None), Some(30));
        assert_eq!(stricter_limit(None, Some(40)), Some(40));
        assert_eq!(stricter_limit(None, None), None);
    }

    #[test]
    fn parses_outgoing_utp_policy() {
        assert_eq!(
            parse_outgoing_transport_policy("auto"),
            OutgoingTransportPolicy::Auto
        );
        assert_eq!(
            parse_outgoing_transport_policy("prefer"),
            OutgoingTransportPolicy::PreferUtp
        );
        assert_eq!(
            parse_outgoing_transport_policy("utp-only"),
            OutgoingTransportPolicy::UtpOnly
        );
        assert_eq!(
            parse_outgoing_transport_policy("off"),
            OutgoingTransportPolicy::TcpOnly
        );
    }

    #[test]
    fn auto_outgoing_utp_policy_is_source_and_privacy_aware() {
        assert_eq!(
            outgoing_transport_policy_for_peer(
                OutgoingTransportPolicy::Auto,
                PeerSource::Tracker,
                false
            ),
            OutgoingTransportPolicy::TcpOnly
        );
        assert_eq!(
            outgoing_transport_policy_for_peer(
                OutgoingTransportPolicy::Auto,
                PeerSource::Dht,
                false
            ),
            OutgoingTransportPolicy::PreferUtp
        );
        assert_eq!(
            outgoing_transport_policy_for_peer(
                OutgoingTransportPolicy::Auto,
                PeerSource::PeerExchange,
                false
            ),
            OutgoingTransportPolicy::PreferUtp
        );
        assert_eq!(
            outgoing_transport_policy_for_peer(
                OutgoingTransportPolicy::UtpOnly,
                PeerSource::Manual,
                true
            ),
            OutgoingTransportPolicy::TcpOnly
        );
    }

    #[test]
    fn super_seed_visible_pieces_reveals_one_available_piece() {
        let addr = "127.0.0.1:6881".parse().unwrap();
        let visible = super_seed_visible_pieces(&[true, true, false, true], addr);

        assert_eq!(visible.iter().filter(|piece| **piece).count(), 1);
        assert!(!visible[2]);
    }

    #[test]
    fn super_seed_visible_pieces_handles_empty_have_set() {
        let addr = "127.0.0.1:6881".parse().unwrap();

        assert_eq!(
            super_seed_visible_pieces(&[false, false, false], addr),
            vec![false, false, false]
        );
    }

    #[test]
    fn webseed_block_url_accepts_direct_file_and_base_url() {
        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.iso".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.iso", false).unwrap(),
                offset: 0,
                pad: false,
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
            comment: None,
            created_by: None,
            creation_date: None,
            name: "payload-dir".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_components(&["payload-dir", "payload.bin"], false)
                    .unwrap(),
                offset: 0,
                pad: false,
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
            peers.added,
            vec![
                "127.0.0.1:6881".parse::<SocketAddr>().unwrap(),
                "10.0.0.2:5000".parse::<SocketAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn parses_ut_pex_added6_ipv6_peers() {
        // TNG-020: added6 (BEP 11 IPv6 compact peers, 16-byte address + 2-byte
        // port) was previously not parsed at all -- only IPv4 `added`.
        let mut added6 = Vec::new();
        added6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        added6.extend_from_slice(&0x1a_e1u16.to_be_bytes());
        added6.extend_from_slice(&Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1).octets());
        added6.extend_from_slice(&0x1388u16.to_be_bytes());
        let payload = rt_bencode::encode(&BValue::Dict(vec![(
            b"added6".as_slice(),
            BValue::Bytes(&added6),
        )]));

        let peers = parse_ut_pex_peers(&payload).unwrap();

        assert_eq!(
            peers.added,
            vec![
                SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 6881, 0, 0)),
                SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                    5000,
                    0,
                    0
                )),
            ]
        );
    }

    #[test]
    fn parses_ut_pex_added_and_added6_together() {
        let added = [10, 0, 0, 2, 0x13, 0x88];
        let mut added6 = Vec::new();
        added6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        added6.extend_from_slice(&0x1a_e1u16.to_be_bytes());
        let payload = rt_bencode::encode(&BValue::Dict(vec![
            (b"added".as_slice(), BValue::Bytes(&added)),
            (b"added6".as_slice(), BValue::Bytes(&added6)),
        ]));

        let peers = parse_ut_pex_peers(&payload).unwrap();

        assert_eq!(
            peers.added,
            vec![
                "10.0.0.2:5000".parse::<SocketAddr>().unwrap(),
                SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 6881, 0, 0)),
            ]
        );
    }

    #[test]
    fn parses_ut_pex_dropped_ipv4_and_ipv6_peers() {
        let dropped = [10, 0, 0, 2, 0x13, 0x88];
        let mut dropped6 = Vec::new();
        dropped6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        dropped6.extend_from_slice(&0x1a_e1u16.to_be_bytes());
        let payload = rt_bencode::encode(&BValue::Dict(vec![
            (b"dropped".as_slice(), BValue::Bytes(&dropped)),
            (b"dropped6".as_slice(), BValue::Bytes(&dropped6)),
        ]));

        let peers = parse_ut_pex_peers(&payload).unwrap();

        assert!(peers.added.is_empty());
        assert_eq!(
            peers.dropped,
            vec![
                "10.0.0.2:5000".parse::<SocketAddr>().unwrap(),
                SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 6881, 0, 0)),
            ]
        );
    }

    #[test]
    fn private_torrent_extension_handshake_does_not_advertise_pex() {
        let metadata = Arc::new(vec![1, 2, 3, 4]);

        let public = extension_handshake_for_torrent(Some(&metadata), false, true);
        let private = extension_handshake_for_torrent(Some(&metadata), true, true);
        let disabled = extension_handshake_for_torrent(Some(&metadata), false, false);

        assert_eq!(public.ut_metadata_id(), Some(LOCAL_UT_METADATA_ID));
        assert_eq!(public.ut_pex_id(), Some(LOCAL_UT_PEX_ID));
        assert_eq!(private.ut_metadata_id(), Some(LOCAL_UT_METADATA_ID));
        assert_eq!(private.ut_pex_id(), None);
        assert_eq!(disabled.ut_metadata_id(), Some(LOCAL_UT_METADATA_ID));
        assert_eq!(disabled.ut_pex_id(), None);
    }

    #[test]
    fn metadata_response_rejects_unrepresentable_or_out_of_range_piece() {
        let metadata = vec![1, 2, 3, 4];

        assert_eq!(
            metadata_response(u32::MAX, &metadata),
            UtMetadataMessage::Reject { piece: u32::MAX }
        );
        assert_eq!(
            metadata_response(1, &metadata),
            UtMetadataMessage::Reject { piece: 1 }
        );
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
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 3,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };

        let hints = collect_file_hints(dir.path(), &meta);

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].file_index, 3);
        assert_eq!(hints[0].size, 5);
        assert!(hints[0].mtime_secs > 0);
    }

    #[test]
    fn file_hints_omit_missing_files_without_failing_the_whole_torrent() {
        // Regression test for the "one missing file poisons the whole
        // torrent's fastresume trust" bug: a multi-file torrent where one
        // file is absent (e.g. a BEP47 padding file real clients never
        // write, or a renamed/deleted file) must still return hints for
        // every OTHER file that does exist on disk.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.bin"), b"hello").unwrap();
        // "missing.bin" is intentionally never created.

        let meta = TorrentMetaV1 {
            info_hash: [1; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "multi".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![
                rt_metainfo::TorrentFileV1 {
                    index: 0,
                    length: 5,
                    path: rt_path::SafeRelPath::from_name("present.bin", false).unwrap(),
                    offset: 0,
                    pad: false,
                },
                rt_metainfo::TorrentFileV1 {
                    index: 1,
                    length: 0,
                    path: rt_path::SafeRelPath::from_name("missing.bin", false).unwrap(),
                    offset: 5,
                    pad: false,
                },
            ],
            private: false,
            raw: Vec::new(),
        };

        let hints = collect_file_hints(dir.path(), &meta);

        assert_eq!(hints.len(), 1, "only the present file should have a hint");
        assert_eq!(hints[0].file_index, 0);
        assert_eq!(hints[0].size, 5);
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
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 5,
                path: rt_path::SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
                pad: false,
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
        let piece_map = Arc::new(PieceMap::new(16 * 1024, files).unwrap());
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
            have_pieces: PieceBitmap::from_bools(&[true]),
            metadata: None,
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: None,
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
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
    fn upload_context_piece_map_is_shared_not_deep_cloned_per_peer() {
        // TNG-014: `UploadContext.piece_map` used to be an owned `PieceMap`,
        // deep-copying its `files: Vec<FileSpan>` on every new peer
        // connection (`upload_context()`'s `self.piece_map.clone()`).
        // Wrapping it in `Arc` makes that the same cheap refcount bump every
        // other per-peer field already gets (`storage`, `resources`, etc.)
        // instead of real, avoidable memory growth at swarm scale.
        let files = vec![FileSpan {
            file_index: 0,
            path: rt_path::SafeRelPath::from_name("a.bin", false).unwrap(),
            content_offset: 0,
            length: 256,
        }];
        let shared = Arc::new(PieceMap::new(16 * 1024, files).unwrap());
        assert_eq!(Arc::strong_count(&shared), 1);

        // Mirrors exactly what `upload_context()` does per new peer
        // connection: `piece_map: self.piece_map.clone()`.
        let per_peer_a = shared.clone();
        let per_peer_b = shared.clone();

        assert_eq!(
            Arc::strong_count(&shared),
            3,
            "cloning for two peer connections should only bump the refcount, not allocate two new PieceMaps"
        );
        assert!(
            Arc::ptr_eq(&shared, &per_peer_a) && Arc::ptr_eq(&per_peer_a, &per_peer_b),
            "every peer's piece_map must point at the exact same allocation"
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

    #[tokio::test]
    async fn peer_event_delivery_is_bounded_when_torrent_actor_stalls() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(PeerEvent::Unchoked {
            peer: "127.0.0.1:6881".parse().unwrap(),
        })
        .await
        .unwrap();

        let started = Instant::now();
        let delivered = send_peer_event(
            &tx,
            PeerEvent::Interested {
                peer: "127.0.0.1:6881".parse().unwrap(),
            },
        )
        .await;

        assert!(!delivered);
        assert!(started.elapsed() < PEER_EVENT_SEND_TIMEOUT + Duration::from_secs(1));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn peer_event_channel_capacity_is_bounded_by_global_peer_budget() {
        assert_eq!(peer_event_channel_capacity(1), 64);
        assert_eq!(peer_event_channel_capacity(200), 200);
        assert_eq!(peer_event_channel_capacity(10_000), 512);
    }
}
