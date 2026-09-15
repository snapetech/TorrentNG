//! Pure BEP 52 torrent task.
//!
//! The original torrent actor is intentionally v1-shaped: its picker and
//! storage map operate on SHA-1 pieces in the concatenated v1 file stream.
//! Pure v2 torrents have a different piece address space (each file starts on
//! a piece boundary) and a SHA-256 merkle verification rule, so keeping this
//! actor separate prevents a v2 compatibility path from silently changing v1
//! semantics.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::StreamExt;
use rt_hash::{merkle_layers, merkle_root, piece_layer_hash, BlockHash, V2_BLOCK_SIZE};
use rt_metainfo::{TorrentFileV2, TorrentMetaV2};
use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_path::{StorageProfile, StorageRootId};
use rt_peer_wire::{
    codec::PeerCodec,
    extension::{ExtensionHandshake, EXT_HANDSHAKE_ID},
    handshake::{ExtensionFlags, Handshake},
    message::Message,
};
use rt_piece_map::{V2FileSpan, V2PieceMap};
use rt_session::{SessionRegistry, TorrentEntry, TorrentState};
use rt_storage::{
    scheduler::{scheduled_read_owned, scheduled_write},
    IoClass, MountScheduler, SchedulerConfig, StorageIoConfig, V2FileHash, V2FileVerifier,
    VerifyResult,
};
use rt_tracker::{TrackerEvent, TrackerState, TrackerStatus};
use rt_utp::UtpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, OwnedSemaphorePermit, RwLock};
use tokio::time::{interval, sleep, timeout};
use tokio_util::codec::Framed;
use tracing::{debug, warn};

use crate::db_worker::DbExecutor;
use crate::egress_policy::OutboundEgressPolicy;
use crate::network_budget::GlobalNetworkBudget;
use crate::torrent_task::{
    outgoing_transport_policy_configured, outgoing_transport_policy_for_peer,
    prepare_tracker_peer_cache, private_peer_source_allowed, restore_tracker_state_from_rows,
    tracker_peer_cache_cap, tracker_tiers_from_urls, OutgoingTransportPolicy, PeerIo, PeerSource,
    TorrentCmd, UtpPeerIo,
};
use crate::tracker_runtime::{
    announce_tracker, protocol_numwant, TrackerAnnounceContext, TrackerAnnounceResult,
    TrackerAnnounceSpec, TrackerWorkers, MAX_TRACKER_ANNOUNCES_IN_FLIGHT,
    STOPPED_TRACKER_ANNOUNCE_DEADLINE,
};
use crate::{EnginePeerSnapshot, EngineTorrentLimits, EngineWebseedSnapshot, TorrentRuntimeStats};

const V2_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const V2_SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const V2_PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const V2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const V2_REQUEST_WINDOW: usize = 32;
// `TorrentMetaV2` accepts every piece length representable by the peer-wire
// u32 coordinates. The old 65,536-block guard silently rejected valid pieces
// above about 1 GiB. Keep a protocol-derived bound instead; the assembly byte
// cap and ResourceGovernor still bound the actual retained content.
const V2_MAX_PIECE_BLOCKS: usize =
    (u32::MAX as usize).div_ceil(rt_peer_wire::message::MAX_BLOCK_SIZE as usize);
const V2_MAX_HASH_EXCHANGE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const V2_MAX_HASH_REQUEST_LENGTH: u32 = 512;
const STOPPED_TRACKER_CONTROL_ANNOUNCE_DEADLINE: Duration = Duration::from_secs(1);

/// Preserve BEP 12's tier failover semantics for pure-v2 metainfo. The flat
/// tracker list is still used for durable client overrides, but an untouched
/// torrent must keep the announce-list's tier boundaries.
fn tracker_tiers_from_meta_v2(meta: &TorrentMetaV2) -> Vec<Vec<TrackerState>> {
    let mut seen = HashSet::new();
    let mut tiers = Vec::new();

    for tier in &meta.announce_list {
        let trackers = tier
            .iter()
            .filter_map(|url| {
                if url.is_empty() || !seen.insert(url.clone()) {
                    None
                } else {
                    Some(TrackerState::new(url.clone()))
                }
            })
            .collect::<Vec<_>>();
        if !trackers.is_empty() {
            tiers.push(trackers);
        }
    }

    if let Some(url) = &meta.announce {
        if !url.is_empty() && seen.insert(url.clone()) {
            tiers.insert(0, vec![TrackerState::new(url.clone())]);
        }
    }
    tiers
}

/// Estimate the v2 piece-index allocation retained by the live task.
pub(crate) fn v2_piece_index_memory_bytes(piece_count: usize) -> usize {
    piece_count
        .div_ceil(64)
        .saturating_mul(std::mem::size_of::<u64>())
}

/// Estimate the immutable v2 graph retained by a live task. The exact parser
/// allocation is deliberately charged before the actor is constructed, just
/// as it is for the v1 actor; this covers paths, file descriptors' metadata,
/// piece-layer vectors, and the retained raw info dictionary.
pub(crate) fn persistent_torrent_metadata_memory_bytes_v2(meta: &TorrentMetaV2) -> usize {
    let mut total = 64 * 1024usize;
    total = total.saturating_add(meta.name.capacity());
    total = total.saturating_add(meta.raw.capacity());
    total = total.saturating_add(meta.announce.as_ref().map_or(0, String::capacity));
    total = total.saturating_add(meta.comment.as_ref().map_or(0, String::capacity));
    total = total.saturating_add(meta.created_by.as_ref().map_or(0, String::capacity));
    total = total.saturating_add(
        meta.announce_list
            .capacity()
            .saturating_mul(std::mem::size_of::<Vec<String>>()),
    );
    for tier in &meta.announce_list {
        total = total.saturating_add(
            tier.capacity()
                .saturating_mul(std::mem::size_of::<String>()),
        );
        for tracker in tier {
            total = total.saturating_add(tracker.capacity());
        }
    }
    total = total.saturating_add(
        meta.webseeds
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
    );
    for webseed in &meta.webseeds {
        total = total.saturating_add(webseed.capacity());
    }
    total = total.saturating_add(
        meta.files
            .capacity()
            .saturating_mul(std::mem::size_of::<TorrentFileV2>()),
    );
    for file in &meta.files {
        total = total.saturating_add(
            file.path
                .components()
                .len()
                .saturating_mul(std::mem::size_of::<String>()),
        );
        for component in file.path.components() {
            total = total.saturating_add(component.capacity());
        }
    }
    total = total.saturating_add(
        meta.piece_layers
            .capacity()
            .saturating_mul(std::mem::size_of::<([u8; 32], Vec<[u8; 32]>)>()),
    );
    for (root, hashes) in &meta.piece_layers {
        let _ = root;
        total = total.saturating_add(
            hashes
                .capacity()
                .saturating_mul(std::mem::size_of::<[u8; 32]>()),
        );
    }
    total
}

#[derive(Debug, Clone)]
struct V2Bitmap {
    len: usize,
    words: Vec<u64>,
}

impl V2Bitmap {
    fn memory_bytes_for_len(len: usize) -> u64 {
        v2_piece_index_memory_bytes(len) as u64
    }

    fn new(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }

    fn all_true(len: usize) -> Self {
        let mut bitmap = Self {
            len,
            words: vec![u64::MAX; len.div_ceil(64)],
        };
        if let Some(last) = bitmap.words.last_mut() {
            if !len.is_multiple_of(64) {
                *last = (1_u64 << (len % 64)) - 1;
            }
        }
        bitmap
    }

    fn from_bitfield(bits: &[u8], piece_count: usize) -> Result<Self, String> {
        if bits.len() != piece_count.div_ceil(8) {
            return Err(format!(
                "v2 bitfield has {} bytes, expected {}",
                bits.len(),
                piece_count.div_ceil(8)
            ));
        }
        if let Some(remainder) = piece_count.checked_rem(8).filter(|value| *value != 0) {
            let unused_mask = (1_u8 << (8 - remainder)) - 1;
            if bits.last().is_some_and(|byte| byte & unused_mask != 0) {
                return Err("v2 bitfield sets bits beyond the piece count".to_owned());
            }
        }
        let mut bitmap = Self::new(piece_count);
        for (byte_index, byte) in bits.iter().copied().enumerate() {
            for bit in 0..8 {
                let piece = byte_index * 8 + bit;
                if piece >= piece_count {
                    break;
                }
                if byte & (0x80 >> bit) != 0 {
                    bitmap.set(piece, true);
                }
            }
        }
        Ok(bitmap)
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, index: usize) -> bool {
        index < self.len && self.words[index / 64] & (1_u64 << (index % 64)) != 0
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
                if index + 1 == self.words.len() && !self.len.is_multiple_of(64) {
                    word & ((1_u64 << (self.len % 64)) - 1)
                } else {
                    *word
                }
            })
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    fn to_bitfield(&self) -> Vec<u8> {
        let mut bits = vec![0_u8; self.len.div_ceil(8)];
        for index in 0..self.len {
            if self.get(index) {
                bits[index / 8] |= 0x80 >> (index % 8);
            }
        }
        bits
    }
}

#[derive(Debug)]
struct V2PeerState {
    remote_have: V2Bitmap,
    choked: bool,
    upload_choked: bool,
    interested: bool,
    downloaded: u64,
    uploaded: u64,
    download_rate: i64,
    upload_rate: i64,
    download_window: u64,
    upload_window: u64,
    rate_window_started: Instant,
    outstanding: usize,
    assembly_buffers: u64,
    assembly_bytes: u64,
}

impl V2PeerState {
    fn new(piece_count: usize) -> Self {
        Self {
            remote_have: V2Bitmap::new(piece_count),
            choked: true,
            upload_choked: true,
            interested: false,
            downloaded: 0,
            uploaded: 0,
            download_rate: 0,
            upload_rate: 0,
            download_window: 0,
            upload_window: 0,
            rate_window_started: Instant::now(),
            outstanding: 0,
            assembly_buffers: 0,
            assembly_bytes: 0,
        }
    }

    fn record_transfer(&mut self, upload: bool, bytes: u64) {
        let now = Instant::now();
        if upload {
            self.uploaded = self.uploaded.saturating_add(bytes);
            self.upload_window = self.upload_window.saturating_add(bytes);
        } else {
            self.downloaded = self.downloaded.saturating_add(bytes);
            self.download_window = self.download_window.saturating_add(bytes);
        }
        let elapsed = now.saturating_duration_since(self.rate_window_started);
        if elapsed >= Duration::from_secs(1) {
            let seconds = elapsed.as_secs_f64().max(0.001);
            self.download_rate = (self.download_window as f64 / seconds).round() as i64;
            self.upload_rate = (self.upload_window as f64 / seconds).round() as i64;
            self.download_window = 0;
            self.upload_window = 0;
            self.rate_window_started = now;
        }
    }

    fn rate(&self, upload: bool) -> i64 {
        if Instant::now().saturating_duration_since(self.rate_window_started)
            > Duration::from_secs(15)
        {
            0
        } else if upload {
            self.upload_rate
        } else {
            self.download_rate
        }
    }
}

struct V2PeerHandle {
    state: Arc<Mutex<V2PeerState>>,
    task: tokio::task::JoinHandle<()>,
    abort: tokio::task::AbortHandle,
    _bitmap_memory_lease: MemoryLease,
}

#[derive(Debug)]
enum V2PeerEvent {
    Downloaded { peer: SocketAddr, bytes: u64 },
    Uploaded { peer: SocketAddr, bytes: u64 },
    Disconnected { peer: SocketAddr },
}

#[derive(Clone)]
struct V2PeerContext {
    info_hash: [u8; 20],
    peer: SocketAddr,
    meta: Arc<TorrentMetaV2>,
    piece_map: Arc<V2PieceMap>,
    save_root: PathBuf,
    storage: MountScheduler,
    resources: ResourceGovernor,
    network_budget: GlobalNetworkBudget,
    local_have: Arc<RwLock<V2Bitmap>>,
    file_policy: Arc<HashMap<u32, (bool, i64)>>,
    assembly_cap_bytes: usize,
    events: mpsc::Sender<V2PeerEvent>,
}

impl V2PeerContext {
    fn file_is_wanted(&self, file_index: u32) -> bool {
        self.file_policy
            .get(&file_index)
            .copied()
            .unwrap_or((true, 1))
            .0
    }

    fn piece_is_wanted(&self, piece: u32) -> bool {
        let Ok(region) = self.piece_map.piece_to_file(piece) else {
            return false;
        };
        !region.pad && self.file_is_wanted(region.file_index)
    }
}

struct V2PieceAssembly {
    region_len: u32,
    received: Vec<bool>,
    received_count: usize,
    data: Option<Vec<u8>>,
    _memory_lease: Option<MemoryLease>,
}

impl V2PieceAssembly {
    fn new(
        piece: u32,
        region_len: u32,
        cap_bytes: usize,
        resources: &ResourceGovernor,
    ) -> anyhow::Result<Self> {
        let block_count =
            (region_len as usize).div_ceil(rt_peer_wire::message::MAX_BLOCK_SIZE as usize);
        if block_count == 0 || block_count > V2_MAX_PIECE_BLOCKS {
            anyhow::bail!("v2 piece {piece} has too many blocks");
        }
        let bookkeeping_bytes = u64::try_from(block_count)
            .map_err(|_| anyhow::anyhow!("v2 piece bookkeeping size overflow"))?;
        let mut received = Vec::new();
        received
            .try_reserve_exact(block_count)
            .map_err(|error| anyhow::anyhow!("v2 piece bookkeeping allocation failed: {error}"))?;
        received.resize(block_count, false);
        let (data, memory_lease) = if cap_bytes > 0 && (region_len as usize) <= cap_bytes {
            let bytes = u64::from(region_len).saturating_add(bookkeeping_bytes);
            let lease = resources
                .try_acquire(MemoryClass::PieceAssembly, bytes)
                .ok_or_else(|| {
                    anyhow::anyhow!("v2 piece assembly allocation denied: {bytes} bytes")
                })?;
            let mut data = Vec::new();
            data.try_reserve_exact(region_len as usize)
                .map_err(|error| anyhow::anyhow!("v2 piece assembly allocation failed: {error}"))?;
            data.resize(region_len as usize, 0);
            (Some(data), Some(lease))
        } else {
            let lease = resources
                .try_acquire(MemoryClass::PieceAssembly, bookkeeping_bytes)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "v2 piece bookkeeping allocation denied: {bookkeeping_bytes} bytes"
                    )
                })?;
            (None, Some(lease))
        };
        Ok(Self {
            region_len,
            received,
            received_count: 0,
            data,
            _memory_lease: memory_lease,
        })
    }

    fn block_index(&self, begin: u32) -> anyhow::Result<usize> {
        let index = begin as usize / rt_peer_wire::message::MAX_BLOCK_SIZE as usize;
        if index >= self.received.len()
            || begin as usize >= self.region_len as usize
            || !(begin as usize).is_multiple_of(rt_peer_wire::message::MAX_BLOCK_SIZE as usize)
        {
            anyhow::bail!("v2 piece block begins outside the piece");
        }
        Ok(index)
    }

    fn accept_block(&mut self, begin: u32, data: &[u8]) -> anyhow::Result<bool> {
        let index = self.block_index(begin)?;
        let expected = (self.region_len as usize)
            .saturating_sub(begin as usize)
            .min(rt_peer_wire::message::MAX_BLOCK_SIZE as usize);
        if data.len() != expected {
            anyhow::bail!(
                "v2 piece block has {} bytes, expected {}",
                data.len(),
                expected
            );
        }
        if self.received[index] {
            if self.data.as_ref().is_some_and(|current| {
                current[begin as usize..begin as usize + data.len()] != *data
            }) {
                anyhow::bail!("conflicting duplicate v2 piece block");
            }
            return Ok(false);
        }
        if let Some(current) = self.data.as_mut() {
            current[begin as usize..begin as usize + data.len()].copy_from_slice(data);
        }
        self.received[index] = true;
        self.received_count += 1;
        Ok(true)
    }

    fn is_complete(&self) -> bool {
        self.received_count == self.received.len()
    }

    fn next_begin(&self) -> Option<u32> {
        self.received
            .iter()
            .position(|received| !received)
            .and_then(|index| {
                u32::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_mul(rt_peer_wire::message::MAX_BLOCK_SIZE))
            })
    }
}

/// Running pure-v2 task. It owns the v2 piece map and local have bitmap while
/// the engine remains the owner of registry and durable projection ordering.
pub struct V2TorrentTask {
    info_hash_hex: String,
    meta: Arc<TorrentMetaV2>,
    metainfo_files: Vec<TorrentFileV2>,
    save_root: PathBuf,
    piece_map: Arc<V2PieceMap>,
    storage: MountScheduler,
    registry: Arc<RwLock<SessionRegistry>>,
    db: DbExecutor,
    resources: ResourceGovernor,
    network_budget: GlobalNetworkBudget,
    cmd_rx: mpsc::Receiver<TorrentCmd>,
    events_tx: mpsc::Sender<V2PeerEvent>,
    events_rx: mpsc::Receiver<V2PeerEvent>,
    local_have: Arc<RwLock<V2Bitmap>>,
    peers: HashMap<SocketAddr, V2PeerHandle>,
    paused: bool,
    initial_state: TorrentState,
    stopped_announced: bool,
    max_peers: usize,
    torrent_max_peers: Option<usize>,
    file_policy: Arc<HashMap<u32, (bool, i64)>>,
    tracker_tiers: Vec<Vec<TrackerState>>,
    active_tracker_tier: usize,
    tracker_event: TrackerEvent,
    tracker_workers: TrackerWorkers,
    listen_port: u16,
    http_timeout: Duration,
    udp_timeout: Duration,
    min_announce_interval: Option<Duration>,
    known_tracker_peers: HashSet<SocketAddr>,
    tracker_peer_cache_drops: u64,
    assembly_cap_bytes: usize,
    egress_policy: OutboundEgressPolicy,
    _piece_index_memory_lease: Option<MemoryLease>,
    _torrent_metadata_memory_lease: Option<MemoryLease>,
    _file_policy_memory_lease: Option<MemoryLease>,
    _tracker_state_memory_lease: Option<MemoryLease>,
    _tracker_peer_cache_memory_lease: Option<MemoryLease>,
}

impl Drop for V2TorrentTask {
    fn drop(&mut self) {
        for peer in self.peers.values() {
            peer.abort.abort();
        }
    }
}

impl V2TorrentTask {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        meta: TorrentMetaV2,
        save_root: PathBuf,
        paused: bool,
        initial_state: TorrentState,
        registry: Arc<RwLock<SessionRegistry>>,
        db: DbExecutor,
        resources: ResourceGovernor,
        cmd_rx: mpsc::Receiver<TorrentCmd>,
        max_peers: usize,
        piece_assembly_cap_bytes: usize,
        storage_io: StorageIoConfig,
        egress_policy: OutboundEgressPolicy,
        listen_port: u16,
        http_timeout_secs: u64,
        udp_timeout_secs: u64,
        min_interval_secs: u64,
        network_budget: GlobalNetworkBudget,
        piece_index_memory_lease: Option<MemoryLease>,
        torrent_metadata_memory_lease: Option<MemoryLease>,
    ) -> Self {
        let info_hash_hex = hex::encode(meta.info_hash_v2);
        let metainfo_files = meta.files.clone();
        let metainfo_trackers = meta.all_trackers();
        let mut tracker_tiers = tracker_tiers_from_meta_v2(&meta);
        let tracker_restore = db
            .run("restore_v2_tracker_state", {
                let info_hash = info_hash_hex.clone();
                move |db| {
                    let (count, bytes) = rt_db::torrent_tracker_snapshot_size(db, &info_hash)
                        .map_err(|error| error.to_string())?;
                    crate::engine::validate_tracker_snapshot_size(count, bytes)?;
                    let rows = rt_db::list_torrent_trackers(db, &info_hash)
                        .map_err(|error| error.to_string())?;
                    let persisted_trackers = rt_db::torrent_tracker_override(db, &info_hash)
                        .map_err(|error| error.to_string())?;
                    Ok((rows, persisted_trackers))
                }
            })
            .await;
        match tracker_restore {
            Ok((rows, persisted_trackers)) => {
                if persisted_trackers
                    .as_ref()
                    .is_some_and(|trackers| trackers.as_slice() != metainfo_trackers.as_slice())
                {
                    tracker_tiers =
                        tracker_tiers_from_urls(persisted_trackers.as_deref().unwrap_or_default());
                }
                restore_tracker_state_from_rows(&mut tracker_tiers, &rows);
            }
            Err(error) => {
                warn!(
                    component = "db",
                    operation = "restore_v2_tracker_state",
                    torrent = %info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to restore pure-v2 tracker state; using fresh tracker session"
                );
            }
        }
        let tracker_state_memory_lease =
            reserve_v2_tracker_state_memory(&resources, &tracker_tiers);
        if tracker_state_memory_lease.is_none() && !tracker_tiers.is_empty() {
            warn!(
                component = "memory",
                operation = "reserve_v2_tracker_state",
                torrent = %info_hash_hex,
                result = "disabled",
                "pure-v2 tracker state disabled because its memory budget was unavailable"
            );
            tracker_tiers.clear();
        }
        let meta = Arc::new(meta);
        let spans = meta
            .files
            .iter()
            .map(|file| V2FileSpan {
                file_index: file.index,
                path: file.path.clone(),
                piece_offset: file.piece_offset,
                length: file.length,
                pad: file.pad,
            })
            .collect();
        let piece_map = Arc::new(
            V2PieceMap::new(meta.piece_length, spans)
                .expect("metainfo parser rejects invalid v2 piece maps"),
        );
        let peer_event_capacity = max_peers.clamp(64, 512);
        let (events_tx, events_rx) = mpsc::channel(peer_event_capacity);
        let local_have = Arc::new(RwLock::new(V2Bitmap::new(piece_map.piece_count as usize)));
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
        let mut task = Self {
            info_hash_hex,
            meta,
            metainfo_files,
            save_root,
            piece_map,
            storage,
            registry,
            db,
            resources,
            network_budget,
            cmd_rx,
            events_tx,
            events_rx,
            local_have,
            peers: HashMap::new(),
            paused,
            initial_state,
            stopped_announced: paused,
            max_peers,
            torrent_max_peers: None,
            file_policy: Arc::new(HashMap::new()),
            tracker_tiers,
            active_tracker_tier: 0,
            tracker_event: TrackerEvent::Started,
            tracker_workers: TrackerWorkers::new(),
            listen_port,
            http_timeout: Duration::from_secs(http_timeout_secs.max(1)),
            udp_timeout: Duration::from_secs(udp_timeout_secs.max(1)),
            min_announce_interval: (min_interval_secs > 0)
                .then(|| Duration::from_secs(min_interval_secs)),
            known_tracker_peers: HashSet::new(),
            tracker_peer_cache_drops: 0,
            assembly_cap_bytes: piece_assembly_cap_bytes,
            egress_policy,
            _piece_index_memory_lease: piece_index_memory_lease,
            _torrent_metadata_memory_lease: torrent_metadata_memory_lease,
            _file_policy_memory_lease: None,
            _tracker_state_memory_lease: tracker_state_memory_lease,
            _tracker_peer_cache_memory_lease: None,
        };
        let (known_tracker_peers, tracker_peer_cache_memory_lease) =
            prepare_tracker_peer_cache(&task.resources, task.max_peers);
        task.known_tracker_peers = known_tracker_peers;
        task._tracker_peer_cache_memory_lease = tracker_peer_cache_memory_lease;
        task.reset_default_file_policy();
        if let Err(error) = task.apply_file_policy_from_db().await {
            warn!(
                component = "torrent_v2",
                operation = "restore_file_policy",
                torrent = %task.info_hash_hex,
                result = "error",
                error = %error,
                "failed to restore pure-v2 file policy; using metainfo paths and default wanted state"
            );
            task.reset_default_file_policy();
        }
        task
    }

    pub async fn run(mut self) {
        let new_have = self.recheck_files().await;
        *self.local_have.write().await = new_have;

        let startup_state = if self.paused {
            match self.initial_state {
                TorrentState::Error => TorrentState::Error,
                TorrentState::Queued => TorrentState::Queued,
                TorrentState::Stopped => TorrentState::Stopped,
                _ => TorrentState::Paused,
            }
        } else if self.is_complete().await {
            TorrentState::Seeding
        } else {
            TorrentState::Downloading
        };
        if let Err(error) = self.persist_runtime(Some(startup_state), None).await {
            warn!(
                component = "torrent_v2",
                operation = "startup_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist pure-v2 startup state"
            );
            return;
        }

        let mut flush_tick = interval(Duration::from_secs(5));
        let mut tracker_tick = interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                command = self.cmd_rx.recv() => {
                    let Some(command) = command else {
                        self.tracker_workers.cancel();
                        self.announce_stopped_with_control_deadline().await;
                        self.shutdown_peers().await;
                        let _ = self.persist_runtime(None, None).await;
                        break;
                    };
                    if self.handle_command(command).await {
                        break;
                    }
                }
                Some(event) = self.events_rx.recv() => {
                    self.handle_peer_event(event).await;
                }
                Some(result) = self.tracker_workers.recv() => {
                    self.handle_tracker_result(result).await;
                }
                _ = flush_tick.tick() => {
                    if !self.paused {
                        let _ = self.persist_runtime(None, None).await;
                    }
                }
                _ = tracker_tick.tick() => {
                    if !self.paused {
                        self.start_due_tracker_announces().await;
                    }
                }
            }
        }
        self.reject_pending_commands();
    }

    async fn handle_command(&mut self, command: TorrentCmd) -> bool {
        match command {
            TorrentCmd::Shutdown => {
                self.tracker_workers.cancel();
                self.announce_stopped_with_control_deadline().await;
                self.shutdown_peers().await;
                let _ = self.persist_runtime(None, None).await;
                true
            }
            TorrentCmd::Pause { reply } => {
                let was_paused = self.paused;
                self.paused = true;
                self.tracker_workers.cancel();
                self.shutdown_peers().await;
                let result = self.persist_runtime(Some(TorrentState::Paused), None).await;
                if result.is_err() {
                    self.paused = was_paused;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                if self.paused {
                    self.announce_stopped_with_control_deadline().await;
                }
                false
            }
            TorrentCmd::Resume { reply } => {
                let result = self.resume_runtime(reply.is_some()).await;
                if let Some(reply) = reply {
                    let _ = reply.send(result.clone());
                }
                if result.is_ok() {
                    self.recheck_and_set_state().await;
                }
                false
            }
            TorrentCmd::QuiesceForStorageMove { reply } => {
                let was_paused = self.paused;
                self.paused = true;
                self.tracker_workers.cancel();
                self.shutdown_peers().await;
                let result = self
                    .persist_runtime(Some(TorrentState::Paused), None)
                    .await
                    .map(|()| was_paused);
                if result.is_err() {
                    self.paused = was_paused;
                }
                let should_announce_stopped = result.as_ref().is_ok_and(|was_paused| !*was_paused);
                let _ = reply.send(result);
                if should_announce_stopped {
                    self.announce_stopped_with_control_deadline().await;
                }
                false
            }
            TorrentCmd::ResumeAfterStorageMove {
                new_save_root,
                resume_paused,
                reply,
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
                }
                if resume_paused {
                    let _ = reply.send(Ok(()));
                } else {
                    self.paused = false;
                    let result = self.resume_runtime(true).await;
                    let _ = reply.send(result.clone());
                    if result.is_ok() {
                        self.recheck_and_set_state().await;
                    }
                }
                false
            }
            TorrentCmd::Recheck { .. } => {
                self.recheck_and_set_state().await;
                false
            }
            TorrentCmd::NewPeers(peers) => {
                if !self.paused {
                    self.connect_peers(peers, PeerSource::Dht).await;
                }
                false
            }
            TorrentCmd::PriorityPeers(peers) => {
                if !self.paused {
                    self.connect_priority_peers(peers).await;
                }
                false
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
                false
            }
            TorrentCmd::AcceptUtpPeer {
                stream,
                peer_addr,
                handshake,
                peer_permit,
            } => {
                if !self.paused {
                    self.accept_utp_peer(*stream, peer_addr, handshake, peer_permit)
                        .await;
                }
                false
            }
            TorrentCmd::BanPeer(peer) => {
                self.remove_peer(peer).await;
                false
            }
            TorrentCmd::EvictBannedPeers => {
                let banned = {
                    let registry = self.registry.read().await;
                    self.peers
                        .keys()
                        .copied()
                        .filter(|peer| registry.is_peer_banned(*peer))
                        .collect::<Vec<_>>()
                };
                for peer in banned {
                    self.remove_peer(peer).await;
                }
                false
            }
            TorrentCmd::GetPeers { reply } => {
                let _ = reply.send(self.peer_snapshots_unbounded());
                false
            }
            TorrentCmd::GetPeerSnapshotCount { reply } => {
                let _ = reply.send(self.peers.len());
                false
            }
            TorrentCmd::GetPeerSnapshots { max_entries, reply } => {
                let result = if max_entries > crate::torrent_task::MAX_PEER_SNAPSHOT_ITEMS {
                    Err(format!(
                        "requested peer snapshot limit {max_entries} exceeds {}",
                        crate::torrent_task::MAX_PEER_SNAPSHOT_ITEMS
                    ))
                } else if self.peers.len() > max_entries {
                    Err(format!(
                        "torrent peer snapshot contains {} peers; maximum is {max_entries}",
                        self.peers.len()
                    ))
                } else {
                    Ok(self.peer_snapshots_unbounded())
                };
                let _ = reply.send(result);
                false
            }
            TorrentCmd::GetWebseeds { reply } => {
                let _ = reply.send(Vec::<EngineWebseedSnapshot>::new());
                false
            }
            TorrentCmd::GetRuntimeStats { reply } => {
                let _ = reply.send(self.runtime_stats());
                false
            }
            TorrentCmd::ReloadFilePolicy { reply } => {
                let result = self.apply_file_policy_from_db().await;
                if result.is_ok() {
                    self.recheck_and_set_state().await;
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                false
            }
            TorrentCmd::UpdateLimits { limits, reply } => {
                self.apply_limits(&limits);
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
                false
            }
            TorrentCmd::UpdateTrackers { trackers, reply } => {
                let result = self.apply_tracker_urls(trackers).await;
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                false
            }
            TorrentCmd::Reannounce => {
                self.tracker_event = TrackerEvent::Empty;
                self.schedule_active_tracker_tier_now();
                false
            }
            TorrentCmd::CancelJob { .. } | TorrentCmd::UpdatePeerExchange(_) => false,
        }
    }

    fn apply_limits(&mut self, limits: &EngineTorrentLimits) {
        self.torrent_max_peers = limits.max_connections.and_then(|value| {
            usize::try_from(value)
                .ok()
                .filter(|connections| *connections > 0)
        });
    }

    fn reset_default_file_policy(&mut self) {
        self.file_policy = Arc::new(
            self.meta
                .files
                .iter()
                .map(|file| (file.index, (!file.pad, 1)))
                .collect(),
        );
    }

    fn reserve_v2_file_policy_memory(
        &self,
        files: &[TorrentFileV2],
    ) -> Result<Option<MemoryLease>, String> {
        let mut bytes = files
            .len()
            .saturating_mul(std::mem::size_of::<TorrentFileV2>());
        for file in files {
            bytes = bytes.saturating_add(
                file.path
                    .components()
                    .len()
                    .saturating_mul(std::mem::size_of::<String>()),
            );
            for component in file.path.components() {
                bytes = bytes.saturating_add(component.capacity());
            }
        }
        if bytes == 0 {
            return Ok(None);
        }
        let bytes = u64::try_from(bytes)
            .map_err(|_| "pure-v2 file policy memory estimate overflows u64".to_owned())?;
        self.resources
            .try_acquire(MemoryClass::Metadata, bytes)
            .map(Some)
            .ok_or_else(|| format!("pure-v2 file policy allocation of {bytes} bytes denied"))
    }

    async fn apply_file_policy_from_db(&mut self) -> Result<(), String> {
        let info_hash = self.info_hash_hex.clone();
        let rows = self
            .db
            .run("load_v2_file_policy", move |db| {
                rt_db::list_torrent_files(db, &info_hash)
                    .map_err(|error| format!("loading pure-v2 file policy: {error}"))
            })
            .await?;
        let mut effective_files = self.metainfo_files.clone();
        let mut policy = HashMap::with_capacity(rows.len());
        for row in rows {
            let file_index = u32::try_from(row.file_index).map_err(|_| {
                format!(
                    "persisted pure-v2 file policy has invalid file index {}",
                    row.file_index
                )
            })?;
            let Some(file) = effective_files
                .iter_mut()
                .find(|file| file.index == file_index)
            else {
                return Err(format!(
                    "persisted pure-v2 file policy references unknown file index {file_index}"
                ));
            };
            file.path = rt_path::SafeRelPath::from_slash_separated(&row.path, cfg!(windows))
                .map_err(|error| {
                    format!(
                        "invalid persisted pure-v2 file path {:?}: {error}",
                        row.path
                    )
                })?;
            if !(0..=2).contains(&row.priority) {
                return Err(format!(
                    "persisted pure-v2 file policy has invalid priority {} for file {file_index}",
                    row.priority
                ));
            }
            if policy
                .insert(file_index, (row.wanted && !file.pad, row.priority))
                .is_some()
            {
                return Err(format!(
                    "persisted pure-v2 file policy contains duplicate file index {file_index}"
                ));
            }
        }

        crate::engine::validate_file_path_projection(
            effective_files.iter().map(|file| file.path.as_display()),
        )?;
        for file in &effective_files {
            policy.entry(file.index).or_insert((!file.pad, 1));
        }

        let policy_changed = self.file_policy.as_ref() != &policy;
        let current_files = self.meta.files.clone();
        let paths_changed = current_files.len() != effective_files.len()
            || current_files
                .iter()
                .zip(&effective_files)
                .any(|(current, effective)| {
                    current.index != effective.index || current.path != effective.path
                });
        if paths_changed {
            let mut changed_file_indices = current_files
                .iter()
                .filter_map(|current| {
                    effective_files
                        .iter()
                        .find(|effective| effective.index == current.index)
                        .filter(|effective| effective.path != current.path)
                        .map(|_| current.index)
                })
                .collect::<Vec<_>>();
            changed_file_indices.sort_unstable();
            changed_file_indices.dedup();
            let changed_piece_ranges = self
                .piece_map
                .piece_ranges_for_file_indices(&changed_file_indices)
                .map_err(|error| format!("invalidating renamed pure-v2 file pieces: {error}"))?;
            let piece_map = Arc::new(
                V2PieceMap::new(
                    self.meta.piece_length,
                    effective_files
                        .iter()
                        .map(|file| V2FileSpan {
                            file_index: file.index,
                            path: file.path.clone(),
                            piece_offset: file.piece_offset,
                            length: file.length,
                            pad: file.pad,
                        })
                        .collect(),
                )
                .map_err(|error| format!("rebuilding pure-v2 file map: {error}"))?,
            );
            let file_policy_memory_lease = self.reserve_v2_file_policy_memory(&effective_files)?;
            self.shutdown_peers().await;
            {
                let mut have = self.local_have.write().await;
                for (first, last) in changed_piece_ranges {
                    for piece in first..last {
                        have.set(piece as usize, false);
                    }
                }
            }
            Arc::make_mut(&mut self.meta).files = effective_files;
            self.piece_map = piece_map;
            self._file_policy_memory_lease = file_policy_memory_lease;
        } else if policy_changed {
            // Peer contexts hold an immutable snapshot of this policy.  Tear
            // them down so reconnects cannot continue requesting files whose
            // wanted/priority state has just changed.
            self.shutdown_peers().await;
        }
        self.file_policy = Arc::new(policy);
        Ok(())
    }

    fn piece_is_wanted(&self, piece: u32) -> bool {
        let Ok(region) = self.piece_map.piece_to_file(piece) else {
            return false;
        };
        !region.pad
            && self
                .file_policy
                .get(&region.file_index)
                .copied()
                .unwrap_or((true, 1))
                .0
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
        let tier_index = self
            .active_tracker_tier
            .min(self.tracker_tiers.len().saturating_sub(1));
        let Some(tier) = self.tracker_tiers.get_mut(tier_index) else {
            return;
        };
        for tracker in tier {
            tracker.schedule_immediate();
        }
    }

    async fn apply_tracker_urls(&mut self, trackers: Vec<String>) -> Result<(), String> {
        let tracker_tiers = tracker_tiers_from_urls(&trackers);
        let tracker_state_memory_lease =
            reserve_v2_tracker_state_memory(&self.resources, &tracker_tiers);
        if !tracker_tiers.is_empty() && tracker_state_memory_lease.is_none() {
            return Err("pure-v2 tracker state allocation denied".to_owned());
        }
        self.tracker_workers.cancel();
        self.known_tracker_peers.clear();
        self.tracker_tiers = tracker_tiers;
        self.active_tracker_tier = 0;
        self.tracker_event = TrackerEvent::Empty;
        self._tracker_state_memory_lease = tracker_state_memory_lease;
        self.schedule_trackers_now();
        {
            let mut registry = self.registry.write().await;
            if let Some(mut entry) = registry.get_mut(&self.info_hash_hex) {
                entry.tracker_message = None;
            };
        }
        self.persist_tracker_state().await
    }

    async fn start_due_tracker_announces(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }
        let tier_idx = self
            .active_tracker_tier
            .min(self.tracker_tiers.len().saturating_sub(1));
        let available = self.tracker_workers.available();
        if available == 0 {
            return;
        }
        let specs = self.tracker_tiers[tier_idx]
            .iter()
            .enumerate()
            .filter(|(index, tracker)| {
                tracker.is_due() && !self.tracker_workers.contains((tier_idx, *index))
            })
            .take(available)
            .map(|(index, tracker)| TrackerAnnounceSpec {
                key: (tier_idx, index),
                url: tracker.url.clone(),
                tracker_id: tracker.tracker_id.clone(),
                event: self.tracker_event,
            })
            .collect::<Vec<_>>();
        if specs.is_empty() {
            return;
        }
        let context = self.tracker_announce_context().await;
        self.tracker_workers.start(specs, context);
    }

    async fn tracker_announce_context(&self) -> TrackerAnnounceContext {
        let (uploaded, downloaded) = self
            .registry
            .read()
            .await
            .get(&self.info_hash_hex)
            .map(|entry| (entry.stats.uploaded, entry.stats.downloaded))
            .unwrap_or((0, 0));
        let have = self.local_have.read().await.clone();
        TrackerAnnounceContext {
            info_hash: self.meta.info_hash_v2[..20]
                .try_into()
                .expect("v2 info hash truncation is 20 bytes"),
            uploaded,
            downloaded,
            left: self.bytes_left(&have),
            listen_port: self.listen_port,
            http_timeout: self.http_timeout,
            udp_timeout: self.udp_timeout,
            numwant: protocol_numwant(self.peer_capacity()),
            egress_policy: self.egress_policy,
            resources: self.resources.clone(),
        }
    }

    async fn handle_tracker_result(&mut self, result: TrackerAnnounceResult) {
        self.tracker_workers.complete(result.key, result.generation);
        if !self.tracker_workers.is_current(result.generation) {
            return;
        }
        let (tier_idx, tracker_idx) = result.key;
        let Some(tracker) = self
            .tracker_tiers
            .get_mut(tier_idx)
            .and_then(|tier| tier.get_mut(tracker_idx))
        else {
            return;
        };
        match result.response {
            Ok(response) => {
                let peers = response
                    .peers
                    .iter()
                    .map(|peer| peer.addr)
                    .collect::<Vec<_>>();
                tracker.on_success_with_min_interval(&response, self.min_announce_interval);
                self.tracker_event =
                    v2_tracker_event_after_success(self.tracker_event, result.event);
                let _ = self.persist_tracker_state().await;
                if !peers.is_empty() && !self.paused {
                    self.remember_tracker_peers(&peers);
                    self.connect_peers(peers, PeerSource::Tracker).await;
                }
            }
            Err(error) => {
                tracker.on_failure(error);
                let _ = self.persist_tracker_state().await;
                if self.tracker_tiers.get(tier_idx).is_some_and(|tier| {
                    !tier.is_empty()
                        && tier
                            .iter()
                            .all(|tracker| matches!(tracker.status, TrackerStatus::Error(_)))
                }) {
                    self.active_tracker_tier = self
                        .active_tracker_tier
                        .saturating_add(1)
                        .min(self.tracker_tiers.len().saturating_sub(1));
                }
            }
        }
    }

    async fn announce_stopped(&mut self) {
        if !consume_v2_stopped_announce(&mut self.stopped_announced) {
            return;
        }
        let candidates = self
            .tracker_tiers
            .iter()
            .enumerate()
            .flat_map(|(tier_index, tier)| {
                tier.iter()
                    .enumerate()
                    .map(move |(tracker_index, tracker)| {
                        (
                            tier_index,
                            tracker_index,
                            tracker.url.clone(),
                            tracker.tracker_id.clone(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return;
        }

        let context = self.tracker_announce_context().await;
        let mut pending = futures::stream::iter(candidates.into_iter().map(
            |(tier_index, tracker_index, url, tracker_id)| {
                let context = context.clone();
                async move {
                    let result = announce_tracker(
                        &context,
                        &url,
                        TrackerEvent::Stopped,
                        tracker_id.as_deref(),
                    )
                    .await
                    .map(|response| response.response);
                    (tier_index, tracker_index, url, result)
                }
            },
        ))
        .buffer_unordered(MAX_TRACKER_ANNOUNCES_IN_FLIGHT);
        let deadline = sleep(STOPPED_TRACKER_ANNOUNCE_DEADLINE);
        tokio::pin!(deadline);
        let mut results = Vec::new();
        loop {
            tokio::select! {
                result = pending.next() => {
                    let Some(result) = result else { break };
                    results.push(result);
                }
                _ = &mut deadline => {
                    warn!(
                        component = "tracker",
                        operation = "announce_stopped",
                        torrent = %self.info_hash_hex,
                        result = "deadline_exceeded",
                        deadline_secs = STOPPED_TRACKER_ANNOUNCE_DEADLINE.as_secs(),
                        completed = results.len(),
                        "pure-v2 stopped tracker announces exceeded aggregate deadline"
                    );
                    break;
                }
            }
        }

        let had_results = !results.is_empty();
        for (tier_index, tracker_index, url, result) in results {
            let Some(tracker) = self
                .tracker_tiers
                .get_mut(tier_index)
                .and_then(|tier| tier.get_mut(tracker_index))
            else {
                continue;
            };
            match result {
                Ok(response) => {
                    tracker.on_success_with_min_interval(&response, self.min_announce_interval);
                }
                Err(error) => {
                    warn!(
                        component = "tracker",
                        operation = "announce_stopped",
                        torrent = %self.info_hash_hex,
                        tracker = %url,
                        result = "error",
                        error = %error,
                        "pure-v2 stopped tracker announce failed"
                    );
                    tracker.on_failure(error);
                }
            }
        }
        if had_results {
            let _ = self.persist_tracker_state().await;
        }
    }

    async fn announce_stopped_with_control_deadline(&mut self) {
        if timeout(
            STOPPED_TRACKER_CONTROL_ANNOUNCE_DEADLINE,
            self.announce_stopped(),
        )
        .await
        .is_err()
        {
            warn!(
                component = "tracker",
                operation = "announce_stopped",
                torrent = %self.info_hash_hex,
                result = "control_deadline_exceeded",
                deadline_ms = STOPPED_TRACKER_CONTROL_ANNOUNCE_DEADLINE.as_millis(),
                "pure-v2 stopped tracker announce exceeded the lifecycle control deadline"
            );
        }
    }

    async fn persist_tracker_state(&self) -> Result<(), String> {
        let (uploaded, downloaded) = {
            let registry = self.registry.read().await;
            registry
                .get(&self.info_hash_hex)
                .map(|entry| (entry.stats.uploaded, entry.stats.downloaded))
                .unwrap_or((0, 0))
        };
        let have = self.local_have.read().await.clone();
        let left = v2_db_i64(self.bytes_left(&have));
        let now = Instant::now();
        let mut rows = Vec::new();
        let mut tracker_index = 0i64;
        let mut tracker_message = None;
        for (tier_index, tier) in self.tracker_tiers.iter().enumerate() {
            for tracker in tier {
                let failure_reason = v2_tracker_failure_reason(&tracker.status);
                let warning_message = v2_tracker_warning_message(&tracker.status);
                if tracker_message.is_none() {
                    tracker_message = failure_reason.clone().or_else(|| warning_message.clone());
                }
                rows.push(rt_db::TorrentTrackerRow {
                    info_hash: self.info_hash_hex.clone(),
                    tracker_index,
                    tier: i64::try_from(tier_index).unwrap_or(i64::MAX),
                    url: tracker.url.clone(),
                    tracker_id: tracker.tracker_id.clone(),
                    status: v2_tracker_status_label(&tracker.status).to_owned(),
                    last_announce_at: v2_instant_to_unix(tracker.last_announce, now),
                    next_announce_at: v2_instant_to_unix(tracker.next_announce, now),
                    last_success_at: v2_instant_to_unix(tracker.last_success, now),
                    failure_reason,
                    warning_message,
                    seeders: tracker.scrape_complete.map(i64::from),
                    leechers: tracker.scrape_incomplete.map(i64::from),
                    completed: tracker.scrape_downloaded.map(i64::from),
                    uploaded: v2_db_i64(uploaded),
                    downloaded: v2_db_i64(downloaded),
                    left_bytes: left,
                });
                tracker_index = tracker_index.saturating_add(1);
            }
        }
        let info_hash = self.info_hash_hex.clone();
        self.db
            .run("persist_v2_tracker_state", move |db| {
                rt_db::replace_torrent_trackers(db, &info_hash, &rows)
                    .map_err(|error| error.to_string())
            })
            .await?;
        if let Some(mut entry) = self.registry.write().await.get_mut(&self.info_hash_hex) {
            entry.tracker_message = tracker_message;
        }
        Ok(())
    }

    async fn resume_runtime(&mut self, _reply_requested: bool) -> Result<(), String> {
        let was_paused = self.paused;
        self.paused = false;
        self.tracker_event = TrackerEvent::Started;
        self.stopped_announced = false;
        self.schedule_trackers_now();
        let result = async {
            let state = self
                .registry
                .read()
                .await
                .get(&self.info_hash_hex)
                .map(|entry| entry.state)
                .ok_or_else(|| {
                    format!(
                        "torrent {} is missing from the registry",
                        self.info_hash_hex
                    )
                })?;
            if state != TorrentState::Checking {
                self.persist_runtime(Some(TorrentState::Checking), None)
                    .await?;
            }
            Ok::<(), String>(())
        }
        .await;
        if result.is_err() {
            self.paused = was_paused;
            self.tracker_event = TrackerEvent::Started;
            self.stopped_announced = was_paused;
            self.tracker_workers.cancel();
        }
        result
    }

    async fn recheck_and_set_state(&mut self) {
        let new_have = self.recheck_files().await;
        *self.local_have.write().await = new_have;
        let target = if self.paused {
            TorrentState::Paused
        } else if self.is_complete().await {
            TorrentState::Seeding
        } else {
            TorrentState::Downloading
        };
        if let Err(error) = self.persist_runtime(Some(target), None).await {
            warn!(
                component = "torrent_v2",
                operation = "recheck_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist pure-v2 recheck state"
            );
        }
    }

    async fn recheck_files(&self) -> V2Bitmap {
        let files = self
            .meta
            .files
            .iter()
            .filter(|file| !file.pad)
            .map(|file| V2FileHash {
                file_index: file.index,
                path: file.path.clone(),
                length: file.length,
                pieces_root: file.pieces_root,
            })
            .collect::<Vec<_>>();
        let verifier = V2FileVerifier::new(&self.save_root, &self.storage, &files);
        let results = verifier.verify_all().await;
        let mut have = V2Bitmap::new(self.piece_map.piece_count as usize);
        // BEP 47 padding is synthetic zero content. Clients implementing the
        // extension need not materialize it, so a missing `.pad/*` path must
        // not make the torrent appear incomplete. Mark its address-space
        // pieces available without passing it through the file verifier.
        for file in self.meta.files.iter().filter(|file| file.pad) {
            if let Ok(ranges) = self.piece_map.piece_ranges_for_file_indices(&[file.index]) {
                for (first, last) in ranges {
                    for piece in first..last {
                        have.set(piece as usize, true);
                    }
                }
            }
        }
        for (file_index, result) in results {
            match result {
                VerifyResult::Valid => {
                    if let Ok(ranges) = self.piece_map.piece_ranges_for_file_indices(&[file_index])
                    {
                        for (first, last) in ranges {
                            for piece in first..last {
                                have.set(piece as usize, true);
                            }
                        }
                    }
                }
                VerifyResult::Invalid | VerifyResult::Missing { .. } => {}
            }
        }
        have
    }

    async fn is_complete(&self) -> bool {
        let have = self.local_have.read().await;
        self.bytes_left(&have) == 0
    }

    fn bytes_left(&self, have: &V2Bitmap) -> u64 {
        (0..self.piece_map.piece_count)
            .filter(|piece| self.piece_is_wanted(*piece) && !have.get(*piece as usize))
            .filter_map(|piece| self.piece_map.piece_len(piece).ok())
            .map(u64::from)
            .sum()
    }

    async fn persist_runtime(
        &self,
        target_state: Option<TorrentState>,
        transfer: Option<(bool, u64)>,
    ) -> Result<(), String> {
        let have = self.local_have.read().await.clone();
        let amount_left = self.bytes_left(&have);
        let (previous, row) = {
            let mut registry = self.registry.write().await;
            let mut entry = registry.get_mut(&self.info_hash_hex).ok_or_else(|| {
                format!(
                    "torrent {} is missing from the registry",
                    self.info_hash_hex
                )
            })?;
            let previous = entry.clone();
            if let Some((upload, bytes)) = transfer {
                if upload {
                    entry.stats.add_upload(bytes);
                } else {
                    entry.stats.add_download(bytes);
                }
            }
            if let Some(state) = target_state {
                entry.transition(state).map_err(|error| error.to_string())?;
            }
            entry.total_length = self.meta.total_length();
            entry.amount_left = amount_left;
            if entry.state == TorrentState::Seeding && entry.completed_at.is_none() {
                entry.completed_at = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
                entry.amount_left = 0;
            }
            let row = crate::engine::row_from_v2_meta(&entry, &self.meta);
            (previous, row)
        };
        let persistence = self
            .db
            .run_batched("persist_v2_torrent_runtime", move |tx| {
                let updated =
                    rt_db::update_runtime_in_tx(tx, &row).map_err(|error| error.to_string())?;
                if !updated {
                    return Err(format!(
                        "torrent {} is missing from the database",
                        row.info_hash
                    ));
                }
                Ok(())
            })
            .await;
        if let Err(error) = persistence {
            if let Some(mut entry) = self.registry.write().await.get_mut(&self.info_hash_hex) {
                restore_entry_runtime(&mut entry, &previous);
            }
            return Err(error);
        }
        Ok(())
    }

    async fn handle_peer_event(&mut self, event: V2PeerEvent) {
        match event {
            V2PeerEvent::Downloaded { peer, bytes } => {
                let complete = self.is_complete().await;
                let target = complete.then_some(TorrentState::Seeding);
                if let Err(error) = self.persist_runtime(target, Some((false, bytes))).await {
                    debug!(
                        component = "torrent_v2",
                        operation = "persist_download",
                        torrent = %self.info_hash_hex,
                        peer = %peer,
                        result = "error",
                        error = %error,
                        "failed to persist pure-v2 download progress"
                    );
                }
            }
            V2PeerEvent::Uploaded { peer, bytes } => {
                if let Err(error) = self.persist_runtime(None, Some((true, bytes))).await {
                    debug!(
                        component = "torrent_v2",
                        operation = "persist_upload",
                        torrent = %self.info_hash_hex,
                        peer = %peer,
                        result = "error",
                        error = %error,
                        "failed to persist pure-v2 upload progress"
                    );
                }
            }
            V2PeerEvent::Disconnected { peer } => {
                self.peers.remove(&peer);
            }
        }
    }

    fn peer_capacity(&self) -> usize {
        self.torrent_max_peers
            .map(|limit| limit.min(self.max_peers))
            .unwrap_or(self.max_peers)
    }

    fn peer_context(&self, peer: SocketAddr) -> V2PeerContext {
        V2PeerContext {
            info_hash: self.meta.info_hash_v2[..20]
                .try_into()
                .expect("v2 info hash truncation is 20 bytes"),
            peer,
            meta: Arc::clone(&self.meta),
            piece_map: Arc::clone(&self.piece_map),
            save_root: self.save_root.clone(),
            storage: self.storage.clone(),
            resources: self.resources.clone(),
            network_budget: self.network_budget.clone(),
            local_have: Arc::clone(&self.local_have),
            file_policy: Arc::clone(&self.file_policy),
            assembly_cap_bytes: self.assembly_cap_bytes,
            events: self.events_tx.clone(),
        }
    }

    async fn connect_peers(&mut self, peers: Vec<SocketAddr>, source: PeerSource) {
        for peer in peers {
            if self.peers.len() >= self.peer_capacity() {
                break;
            }
            if self.peers.contains_key(&peer)
                || self.registry.read().await.is_peer_banned(peer)
                || !private_peer_source_allowed(self.meta.private, &self.known_tracker_peers, peer)
            {
                continue;
            }
            let Ok(peer_permit) = self.network_budget.try_acquire_peer() else {
                break;
            };
            let bitmap_bytes = V2Bitmap::memory_bytes_for_len(self.piece_map.piece_count as usize);
            let Some(bitmap_memory_lease) = self
                .resources
                .try_acquire(MemoryClass::PeerBuffer, bitmap_bytes)
            else {
                drop(peer_permit);
                break;
            };
            let state = Arc::new(Mutex::new(V2PeerState::new(
                self.piece_map.piece_count as usize,
            )));
            let context = self.peer_context(peer);
            let event_peer = peer;
            let task_state = Arc::clone(&state);
            let transport_policy = outgoing_transport_policy_for_peer(
                outgoing_transport_policy_configured(),
                source,
                self.meta.private,
            );
            let task = tokio::spawn(async move {
                let _peer_permit = peer_permit;
                if let Err(error) =
                    run_outgoing_v2_peer(context.clone(), peer, task_state, transport_policy).await
                {
                    debug!(
                        component = "torrent_v2",
                        operation = "connect_peer",
                        peer = %peer,
                        result = "ended",
                        error = %error,
                        "pure-v2 peer connection ended"
                    );
                }
                let _ = context
                    .events
                    .send(V2PeerEvent::Disconnected { peer: event_peer })
                    .await;
            });
            let abort = task.abort_handle();
            self.peers.insert(
                peer,
                V2PeerHandle {
                    state,
                    task,
                    abort,
                    _bitmap_memory_lease: bitmap_memory_lease,
                },
            );
        }
    }

    async fn connect_priority_peers(&mut self, peers: Vec<SocketAddr>) {
        let preferred = peers
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for peer in peers {
            if self.peers.len() >= self.peer_capacity() {
                let victim = self
                    .peers
                    .iter()
                    .find(|(addr, state)| {
                        !preferred.contains(addr)
                            && state.state.lock().is_ok_and(|state| state.outstanding == 0)
                    })
                    .map(|(addr, _)| *addr);
                if let Some(victim) = victim {
                    self.remove_peer(victim).await;
                }
            }
            if self.peers.len() >= self.peer_capacity() {
                break;
            }
            self.connect_peers(vec![peer], PeerSource::Manual).await;
        }
    }

    fn remember_tracker_peers(&mut self, peers: &[SocketAddr]) {
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
            // The shared cache constructor reserves the complete bounded
            // capacity before any tracker response is accepted, so this
            // insert cannot grow an uncharged HashSet allocation.
            self.known_tracker_peers.insert(peer);
        }
        self.tracker_peer_cache_drops = self.tracker_peer_cache_drops.saturating_add(dropped);
    }

    async fn accept_peer(
        &mut self,
        stream: TcpStream,
        peer: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    ) {
        if handshake.info_hash != self.meta.info_hash_v2[..20]
            || !handshake.reserved.supports_v2()
            || handshake.peer_id == crate::peer_id::our_peer_id()
            || self.peers.len() >= self.peer_capacity()
            || self.peers.contains_key(&peer)
            || self.registry.read().await.is_peer_banned(peer)
            || !private_peer_source_allowed(self.meta.private, &self.known_tracker_peers, peer)
        {
            drop(peer_permit);
            return;
        }
        let bitmap_bytes = V2Bitmap::memory_bytes_for_len(self.piece_map.piece_count as usize);
        let Some(bitmap_memory_lease) = self
            .resources
            .try_acquire(MemoryClass::PeerBuffer, bitmap_bytes)
        else {
            drop(peer_permit);
            return;
        };
        let state = Arc::new(Mutex::new(V2PeerState::new(
            self.piece_map.piece_count as usize,
        )));
        let context = self.peer_context(peer);
        let task_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let _peer_permit = peer_permit;
            if let Err(error) =
                run_incoming_v2_peer(context.clone(), stream, peer, handshake, task_state).await
            {
                debug!(
                    component = "torrent_v2",
                    operation = "accept_peer",
                    peer = %peer,
                    result = "ended",
                    error = %error,
                    "pure-v2 inbound peer ended"
                );
            }
            let _ = context
                .events
                .send(V2PeerEvent::Disconnected { peer })
                .await;
        });
        let abort = task.abort_handle();
        self.peers.insert(
            peer,
            V2PeerHandle {
                state,
                task,
                abort,
                _bitmap_memory_lease: bitmap_memory_lease,
            },
        );
    }

    async fn accept_utp_peer(
        &mut self,
        stream: UtpStream,
        peer: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    ) {
        if handshake.info_hash != self.meta.info_hash_v2[..20]
            || !handshake.reserved.supports_v2()
            || handshake.peer_id == crate::peer_id::our_peer_id()
            || self.peers.len() >= self.peer_capacity()
            || self.peers.contains_key(&peer)
            || self.registry.read().await.is_peer_banned(peer)
            || !private_peer_source_allowed(self.meta.private, &self.known_tracker_peers, peer)
        {
            drop(peer_permit);
            return;
        }
        let bitmap_bytes = V2Bitmap::memory_bytes_for_len(self.piece_map.piece_count as usize);
        let Some(bitmap_memory_lease) = self
            .resources
            .try_acquire(MemoryClass::PeerBuffer, bitmap_bytes)
        else {
            drop(peer_permit);
            return;
        };
        let state = Arc::new(Mutex::new(V2PeerState::new(
            self.piece_map.piece_count as usize,
        )));
        let context = self.peer_context(peer);
        let task_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let _peer_permit = peer_permit;
            if let Err(error) =
                run_incoming_v2_utp_peer(context.clone(), stream, peer, handshake, task_state).await
            {
                debug!(
                    component = "torrent_v2",
                    operation = "accept_utp_peer",
                    peer = %peer,
                    result = "ended",
                    error = %error,
                    "pure-v2 inbound uTP peer ended"
                );
            }
            let _ = context
                .events
                .send(V2PeerEvent::Disconnected { peer })
                .await;
        });
        let abort = task.abort_handle();
        self.peers.insert(
            peer,
            V2PeerHandle {
                state,
                task,
                abort,
                _bitmap_memory_lease: bitmap_memory_lease,
            },
        );
    }

    async fn remove_peer(&mut self, peer: SocketAddr) {
        let Some(handle) = self.peers.remove(&peer) else {
            return;
        };
        handle.abort.abort();
        let _ = handle.task.await;
    }

    async fn shutdown_peers(&mut self) {
        let peers = std::mem::take(&mut self.peers);
        for handle in peers.values() {
            handle.abort.abort();
        }
        for (_, handle) in peers {
            let _ = handle.task.await;
        }
    }

    fn peer_snapshots_unbounded(&self) -> Vec<EnginePeerSnapshot> {
        self.peers
            .iter()
            .filter_map(|(addr, peer)| {
                let state = peer.state.lock().ok()?;
                let pieces = state.remote_have.count_ones();
                let pieces_total = state.remote_have.len();
                Some(EnginePeerSnapshot {
                    addr: *addr,
                    client: "BEP 52 peer".to_owned(),
                    choked: state.choked,
                    upload_choked: state.upload_choked,
                    interested: state.interested,
                    pieces,
                    pieces_total,
                    progress: if pieces_total == 0 {
                        0.0
                    } else {
                        pieces as f64 / pieces_total as f64
                    },
                    download_rate: state.rate(false),
                    upload_rate: state.rate(true),
                    downloaded: state.downloaded,
                    uploaded: state.uploaded,
                })
            })
            .collect()
    }

    fn runtime_stats(&self) -> TorrentRuntimeStats {
        let mut outstanding = 0u64;
        let mut download_rate = 0i64;
        let mut upload_rate = 0i64;
        let mut piece_assembly_buffers = 0u64;
        let mut piece_assembly_bytes = 0u64;
        for peer in self.peers.values() {
            if let Ok(state) = peer.state.lock() {
                outstanding = outstanding.saturating_add(state.outstanding as u64);
                download_rate = download_rate.saturating_add(state.rate(false));
                upload_rate = upload_rate.saturating_add(state.rate(true));
                piece_assembly_buffers =
                    piece_assembly_buffers.saturating_add(state.assembly_buffers);
                piece_assembly_bytes = piece_assembly_bytes.saturating_add(state.assembly_bytes);
            }
        }
        TorrentRuntimeStats {
            connected_peers: self.peers.len() as u64,
            outstanding_requests: outstanding,
            download_rate,
            upload_rate,
            piece_assembly_buffers,
            piece_assembly_bytes,
            tracker_peer_cache_entries: self.known_tracker_peers.len() as u64,
            tracker_peer_cache_drops: self.tracker_peer_cache_drops,
            tracker_peer_cache_bytes: self
                ._tracker_peer_cache_memory_lease
                .as_ref()
                .map_or(0, MemoryLease::bytes),
            storage: self.storage.stats(),
            ..TorrentRuntimeStats::default()
        }
    }

    fn reject_pending_commands(&mut self) {
        while let Ok(command) = self.cmd_rx.try_recv() {
            reject_v2_command(command);
        }
    }
}

fn restore_entry_runtime(entry: &mut TorrentEntry, previous: &TorrentEntry) {
    entry.state = previous.state;
    entry.total_length = previous.total_length;
    entry.amount_left = previous.amount_left;
    entry.stats = previous.stats.clone();
    entry.completed_at = previous.completed_at;
    entry.error_message = previous.error_message.clone();
    entry.tracker_message = previous.tracker_message.clone();
}

fn reserve_v2_tracker_state_memory(
    resources: &ResourceGovernor,
    tiers: &[Vec<TrackerState>],
) -> Option<MemoryLease> {
    let bytes = tiers
        .iter()
        .map(|tier| {
            std::mem::size_of::<Vec<TrackerState>>()
                .saturating_add(
                    tier.capacity()
                        .saturating_mul(std::mem::size_of::<TrackerState>()),
                )
                .saturating_add(
                    tier.iter()
                        .map(|tracker| tracker.url.capacity().saturating_add(32 * 1024))
                        .sum::<usize>(),
                )
        })
        .sum::<usize>();
    if bytes == 0 {
        return None;
    }
    resources.try_acquire(
        MemoryClass::Metadata,
        u64::try_from(bytes).unwrap_or(u64::MAX),
    )
}

fn v2_db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn v2_tracker_event_after_success(current: TrackerEvent, sent: TrackerEvent) -> TrackerEvent {
    if current == sent && matches!(sent, TrackerEvent::Started | TrackerEvent::Completed) {
        TrackerEvent::Empty
    } else {
        current
    }
}

fn consume_v2_stopped_announce(stopped_announced: &mut bool) -> bool {
    if *stopped_announced {
        false
    } else {
        *stopped_announced = true;
        true
    }
}

fn v2_tracker_status_label(status: &TrackerStatus) -> &'static str {
    match status {
        TrackerStatus::NeverAnnounced => "never_announced",
        TrackerStatus::Announcing => "announcing",
        TrackerStatus::Working => "working",
        TrackerStatus::Warning(_) => "warning",
        TrackerStatus::Error(_) => "error",
        TrackerStatus::Disabled => "disabled",
    }
}

fn v2_tracker_failure_reason(status: &TrackerStatus) -> Option<String> {
    match status {
        TrackerStatus::Error(error) => Some(error.to_string()),
        _ => None,
    }
}

fn v2_tracker_warning_message(status: &TrackerStatus) -> Option<String> {
    match status {
        TrackerStatus::Warning(message) => Some(message.clone()),
        _ => None,
    }
}

fn v2_instant_to_unix(instant: Option<Instant>, now: Instant) -> Option<i64> {
    let current = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    instant.map(|instant| {
        if instant >= now {
            current.saturating_add(instant.duration_since(now).as_secs() as i64)
        } else {
            current.saturating_sub(now.duration_since(instant).as_secs() as i64)
        }
    })
}

fn reject_v2_command(command: TorrentCmd) {
    const ERROR: &str = "torrent task is shutting down";
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
            let _ = reply.send(TorrentRuntimeStats::default());
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

async fn run_outgoing_v2_peer(
    context: V2PeerContext,
    addr: SocketAddr,
    state: Arc<Mutex<V2PeerState>>,
    transport_policy: OutgoingTransportPolicy,
) -> anyhow::Result<()> {
    if matches!(
        transport_policy,
        OutgoingTransportPolicy::PreferUtp | OutgoingTransportPolicy::UtpOnly
    ) {
        match run_outgoing_v2_utp_peer(context.clone(), addr, Arc::clone(&state)).await {
            Ok(()) => return Ok(()),
            Err(error) if transport_policy == OutgoingTransportPolicy::PreferUtp => {
                debug!(
                    component = "torrent_v2",
                    operation = "connect_utp",
                    peer = %addr,
                    result = "fallback",
                    error = %error,
                    "pure-v2 uTP setup failed; falling back to TCP"
                );
            }
            Err(error) => return Err(error),
        }
    }
    let stream = timeout(V2_HANDSHAKE_TIMEOUT, TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;
    let mut framed = Framed::with_capacity(
        stream,
        PeerCodec::with_resources(context.resources.clone()),
        1,
    );
    let our_handshake = Handshake {
        info_hash: context.info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_v2_support(),
    };
    write_handshake(&mut framed, &our_handshake).await?;
    let remote = read_handshake(&mut framed).await?;
    validate_remote_v2_handshake(&remote, context.info_hash, addr)?;
    run_v2_peer_protocol(
        context,
        addr,
        PeerIo::Tcp(framed),
        state,
        remote.reserved.supports_extension_protocol(),
        remote.reserved.supports_fast_extension(),
    )
    .await
}

async fn run_outgoing_v2_utp_peer(
    context: V2PeerContext,
    addr: SocketAddr,
    state: Arc<Mutex<V2PeerState>>,
) -> anyhow::Result<()> {
    let mut stream = timeout(V2_HANDSHAKE_TIMEOUT, UtpStream::connect(addr)).await??;
    let our_handshake = Handshake {
        info_hash: context.info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_v2_support(),
    };
    timeout(
        V2_SOCKET_WRITE_TIMEOUT,
        stream.write_all(&our_handshake.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("v2 uTP handshake write timed out"))??;
    let mut bytes = [0u8; rt_peer_wire::handshake::HANDSHAKE_LEN];
    timeout(V2_HANDSHAKE_TIMEOUT, stream.read_exact(&mut bytes)).await??;
    let remote = Handshake::parse(&bytes)?;
    validate_remote_v2_handshake(&remote, context.info_hash, addr)?;
    run_v2_peer_protocol(
        context.clone(),
        addr,
        PeerIo::Utp(Box::new(UtpPeerIo::new(stream, context.resources.clone()))),
        state,
        remote.reserved.supports_extension_protocol(),
        remote.reserved.supports_fast_extension(),
    )
    .await
}

async fn run_incoming_v2_peer(
    context: V2PeerContext,
    stream: TcpStream,
    addr: SocketAddr,
    remote: Handshake,
    state: Arc<Mutex<V2PeerState>>,
) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::with_capacity(
        stream,
        PeerCodec::with_resources(context.resources.clone()),
        1,
    );
    let our_handshake = Handshake {
        info_hash: context.info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_v2_support(),
    };
    write_handshake(&mut framed, &our_handshake).await?;
    validate_remote_v2_handshake(&remote, context.info_hash, addr)?;
    run_v2_peer_protocol(
        context,
        addr,
        PeerIo::Tcp(framed),
        state,
        remote.reserved.supports_extension_protocol(),
        remote.reserved.supports_fast_extension(),
    )
    .await
}

async fn run_incoming_v2_utp_peer(
    context: V2PeerContext,
    mut stream: UtpStream,
    addr: SocketAddr,
    remote: Handshake,
    state: Arc<Mutex<V2PeerState>>,
) -> anyhow::Result<()> {
    validate_remote_v2_handshake(&remote, context.info_hash, addr)?;
    let our_handshake = Handshake {
        info_hash: context.info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_v2_support(),
    };
    timeout(
        V2_SOCKET_WRITE_TIMEOUT,
        stream.write_all(&our_handshake.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("v2 uTP handshake write timed out"))??;
    run_v2_peer_protocol(
        context.clone(),
        addr,
        PeerIo::Utp(Box::new(UtpPeerIo::new(stream, context.resources.clone()))),
        state,
        remote.reserved.supports_extension_protocol(),
        remote.reserved.supports_fast_extension(),
    )
    .await
}

async fn write_handshake(
    framed: &mut Framed<TcpStream, PeerCodec>,
    handshake: &Handshake,
) -> anyhow::Result<()> {
    timeout(
        V2_SOCKET_WRITE_TIMEOUT,
        framed.get_mut().write_all(&handshake.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("v2 peer handshake write timed out"))??;
    Ok(())
}

async fn read_handshake(framed: &mut Framed<TcpStream, PeerCodec>) -> anyhow::Result<Handshake> {
    let mut bytes = [0u8; rt_peer_wire::handshake::HANDSHAKE_LEN];
    timeout(
        V2_HANDSHAKE_TIMEOUT,
        framed.get_mut().read_exact(&mut bytes),
    )
    .await??;
    Handshake::parse(&bytes).map_err(Into::into)
}

fn validate_remote_v2_handshake(
    handshake: &Handshake,
    info_hash: [u8; 20],
    addr: SocketAddr,
) -> anyhow::Result<()> {
    if handshake.info_hash != info_hash {
        anyhow::bail!("v2 peer info_hash mismatch from {addr}");
    }
    if !handshake.reserved.supports_v2() {
        anyhow::bail!("peer {addr} does not advertise BEP 52 support");
    }
    if handshake.peer_id == crate::peer_id::our_peer_id() {
        anyhow::bail!("v2 peer handshake identifies this client");
    }
    Ok(())
}

async fn run_v2_peer_protocol(
    context: V2PeerContext,
    addr: SocketAddr,
    mut framed: PeerIo,
    state: Arc<Mutex<V2PeerState>>,
    remote_supports_extension: bool,
    remote_supports_fast: bool,
) -> anyhow::Result<()> {
    if remote_supports_extension {
        framed
            .send(Message::Extended {
                ext_id: EXT_HANDSHAKE_ID,
                payload: ExtensionHandshake::new(None).encode(),
            })
            .await?;
    }
    send_v2_have(&context, &mut framed, remote_supports_fast).await?;

    let mut outstanding = HashMap::<(u32, u32), u32>::new();
    let mut assemblies = HashMap::<u32, V2PieceAssembly>::new();
    let mut remote_availability_known = false;
    let mut local_interested = false;
    update_v2_interest(
        &context,
        &mut framed,
        &state,
        &mut local_interested,
        remote_availability_known,
    )
    .await?;
    let mut keepalive = interval(V2_KEEPALIVE_INTERVAL);
    let mut last_activity = Instant::now();

    loop {
        tokio::select! {
            message = framed.next() => {
                let message = message?;
                let Some(message) = message else {
                    return Ok(());
                };
                last_activity = Instant::now();
                match message {
                    Message::Bitfield(bits) => {
                        let remote_have = V2Bitmap::from_bitfield(
                            &bits,
                            context.piece_map.piece_count as usize,
                        )
                        .map_err(anyhow::Error::msg)?;
                        if let Ok(mut peer) = state.lock() {
                            peer.remote_have = remote_have;
                        }
                        remote_availability_known = true;
                        update_v2_interest(
                            &context,
                            &mut framed,
                            &state,
                            &mut local_interested,
                            remote_availability_known,
                        ).await?;
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::Have(piece) => {
                        if piece >= context.piece_map.piece_count {
                            continue;
                        }
                        if let Ok(mut peer) = state.lock() {
                            peer.remote_have.set(piece as usize, true);
                        }
                        remote_availability_known = true;
                        update_v2_interest(
                            &context,
                            &mut framed,
                            &state,
                            &mut local_interested,
                            remote_availability_known,
                        ).await?;
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::HaveAll => {
                        if let Ok(mut peer) = state.lock() {
                            peer.remote_have = V2Bitmap::all_true(context.piece_map.piece_count as usize);
                        }
                        remote_availability_known = true;
                        update_v2_interest(
                            &context,
                            &mut framed,
                            &state,
                            &mut local_interested,
                            remote_availability_known,
                        ).await?;
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::HaveNone => {
                        if let Ok(mut peer) = state.lock() {
                            peer.remote_have = V2Bitmap::new(context.piece_map.piece_count as usize);
                        }
                        remote_availability_known = true;
                        update_v2_interest(
                            &context,
                            &mut framed,
                            &state,
                            &mut local_interested,
                            remote_availability_known,
                        ).await?;
                    }
                    Message::Unchoke => {
                        if let Ok(mut peer) = state.lock() {
                            peer.choked = false;
                        }
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::Choke => {
                        outstanding.clear();
                        assemblies.clear();
                        if let Ok(mut peer) = state.lock() {
                            peer.choked = true;
                            peer.outstanding = 0;
                            peer.assembly_buffers = 0;
                            peer.assembly_bytes = 0;
                        }
                    }
                    Message::Interested => {
                        let should_unchoke = if let Ok(mut peer) = state.lock() {
                            peer.interested = true;
                            if peer.upload_choked {
                                peer.upload_choked = false;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if should_unchoke {
                            framed.send(Message::Unchoke).await?;
                        }
                    }
                    Message::NotInterested => {
                        if let Ok(mut peer) = state.lock() {
                            peer.interested = false;
                        }
                    }
                    Message::Request { piece, begin, length } => {
                        serve_v2_request(&context, &mut framed, &state, piece, begin, length).await?;
                    }
                    Message::Cancel { .. } => {}
                    Message::Reject { piece, begin, length } => {
                        if outstanding
                            .get(&(piece, begin))
                            .is_some_and(|expected| *expected == length)
                        {
                            outstanding.remove(&(piece, begin));
                        }
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::Piece { piece, begin, data } => {
                        handle_v2_piece(
                            &context,
                            &mut framed,
                            &state,
                            &mut outstanding,
                            &mut assemblies,
                            piece,
                            begin,
                            data,
                        ).await?;
                        update_v2_interest(
                            &context,
                            &mut framed,
                            &state,
                            &mut local_interested,
                            remote_availability_known,
                        ).await?;
                        fill_v2_requests(&context, &mut framed, &state, &mut outstanding, &mut assemblies).await?;
                    }
                    Message::HashRequest { pieces_root, base_layer, index, length, proof_layers } => {
                        serve_v2_hash_request(
                            &context,
                            &mut framed,
                            pieces_root,
                            base_layer,
                            index,
                            length,
                            proof_layers,
                        ).await?;
                    }
                    Message::Hashes { .. } | Message::HashReject { .. } => {}
                    Message::Extended { ext_id: EXT_HANDSHAKE_ID, payload } => {
                        let _ = ExtensionHandshake::parse(&payload);
                    }
                    Message::Extended { .. } | Message::KeepAlive => {}
                }
            }
            _ = keepalive.tick() => {
                if last_activity.elapsed() >= V2_PEER_IDLE_TIMEOUT {
                    anyhow::bail!("v2 peer {addr} idle timeout");
                }
                framed.send(Message::KeepAlive).await?;
            }
        }
    }
}

async fn send_v2_have(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    remote_supports_fast: bool,
) -> anyhow::Result<()> {
    let have = context.local_have.read().await.clone();
    let count = have.count_ones();
    if remote_supports_fast && have.len() > 0 && count == have.len() {
        framed.send(Message::HaveAll).await?;
    } else if remote_supports_fast && count == 0 {
        framed.send(Message::HaveNone).await?;
    } else if count > 0 {
        framed.send(Message::Bitfield(have.to_bitfield())).await?;
    }
    Ok(())
}

/// Keep the local interest bit synchronized with the pieces this peer can
/// currently provide. Before the first availability message, retain the
/// normal optimistic `Interested` state; once a bitfield/HaveNone is known,
/// withdraw interest when no wanted piece can be requested.
async fn update_v2_interest(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    state: &Arc<Mutex<V2PeerState>>,
    local_interested: &mut bool,
    remote_availability_known: bool,
) -> anyhow::Result<()> {
    let local_have = context.local_have.read().await.clone();
    let has_missing_wanted = (0..context.piece_map.piece_count)
        .any(|piece| context.piece_is_wanted(piece) && !local_have.get(piece as usize));
    let remote_have = state
        .lock()
        .map(|peer| peer.remote_have.clone())
        .unwrap_or_else(|_| V2Bitmap::new(context.piece_map.piece_count as usize));
    let should_be_interested = has_missing_wanted
        && (!remote_availability_known
            || (0..context.piece_map.piece_count).any(|piece| {
                remote_have.get(piece as usize)
                    && context.piece_is_wanted(piece)
                    && !local_have.get(piece as usize)
            }));
    if should_be_interested == *local_interested {
        return Ok(());
    }
    framed
        .send(if should_be_interested {
            Message::Interested
        } else {
            Message::NotInterested
        })
        .await?;
    *local_interested = should_be_interested;
    Ok(())
}

async fn fill_v2_requests(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    state: &Arc<Mutex<V2PeerState>>,
    outstanding: &mut HashMap<(u32, u32), u32>,
    assemblies: &mut HashMap<u32, V2PieceAssembly>,
) -> anyhow::Result<()> {
    let choked = state.lock().map(|peer| peer.choked).unwrap_or(true);
    if choked {
        return Ok(());
    }
    let remote_have = state
        .lock()
        .map(|peer| peer.remote_have.clone())
        .unwrap_or_else(|_| V2Bitmap::new(context.piece_map.piece_count as usize));
    let local_have = context.local_have.read().await.clone();
    while outstanding.len() < V2_REQUEST_WINDOW {
        let mut selected = None;
        for piece in 0..context.piece_map.piece_count {
            let piece_index = piece as usize;
            if local_have.get(piece_index) || !remote_have.get(piece_index) {
                continue;
            }
            let Ok(region) = context.piece_map.piece_to_file(piece) else {
                continue;
            };
            if region.pad || !context.file_is_wanted(region.file_index) {
                continue;
            }
            let assembly = if let Some(assembly) = assemblies.get(&piece) {
                if assembly.is_complete() {
                    continue;
                }
                None
            } else {
                Some(V2PieceAssembly::new(
                    piece,
                    region.length,
                    context.assembly_cap_bytes,
                    &context.resources,
                )?)
            };
            if let Some(assembly) = assembly {
                assemblies.insert(piece, assembly);
            }
            let begin = assemblies
                .get(&piece)
                .and_then(V2PieceAssembly::next_begin)
                .ok_or_else(|| anyhow::anyhow!("v2 piece has no missing block"))?;
            if outstanding.contains_key(&(piece, begin)) {
                continue;
            }
            let length = context
                .piece_map
                .validate_request(piece, begin, rt_peer_wire::message::MAX_BLOCK_SIZE)
                .map(|region| region.length)
                .or_else(|_| {
                    context
                        .piece_map
                        .piece_len(piece)
                        .ok()
                        .and_then(|length| length.checked_sub(begin))
                        .ok_or(rt_piece_map::PieceMapError::ZeroRequestLength)
                })?;
            selected = Some((piece, begin, length));
            break;
        }
        let Some((piece, begin, length)) = selected else {
            break;
        };
        framed
            .send(Message::Request {
                piece,
                begin,
                length,
            })
            .await?;
        outstanding.insert((piece, begin), length);
    }
    if let Ok(mut peer) = state.lock() {
        peer.outstanding = outstanding.len();
        peer.assembly_buffers = assemblies.len() as u64;
        peer.assembly_bytes = assemblies
            .values()
            .filter_map(|assembly| assembly.data.as_ref().map(|data| data.len() as u64))
            .sum();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_v2_piece(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    state: &Arc<Mutex<V2PeerState>>,
    outstanding: &mut HashMap<(u32, u32), u32>,
    assemblies: &mut HashMap<u32, V2PieceAssembly>,
    piece: u32,
    begin: u32,
    data: Bytes,
) -> anyhow::Result<()> {
    let Some(expected) = outstanding.remove(&(piece, begin)) else {
        anyhow::bail!("unsolicited v2 piece block");
    };
    if expected != data.len() as u32 {
        anyhow::bail!("v2 piece response length does not match request");
    }
    let region = context
        .piece_map
        .validate_request(piece, begin, expected)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    if let std::collections::hash_map::Entry::Vacant(entry) = assemblies.entry(piece) {
        entry.insert(V2PieceAssembly::new(
            piece,
            region.length,
            context.assembly_cap_bytes,
            &context.resources,
        )?);
    }
    let (accepted, complete, in_memory) = {
        let assembly = assemblies
            .get_mut(&piece)
            .ok_or_else(|| anyhow::anyhow!("v2 piece assembly disappeared"))?;
        let accepted = assembly.accept_block(begin, &data)?;
        (accepted, assembly.is_complete(), assembly.data.is_some())
    };
    if !accepted {
        return Ok(());
    }
    context
        .network_budget
        .download()
        .acquire(u64::from(expected))
        .await;
    if !in_memory {
        let path = region.path.resolve(&context.save_root);
        scheduled_write(
            &context.storage,
            IoClass::PeerWrite,
            &path,
            region.file_offset,
            data.clone(),
            true,
        )
        .await?;
    }
    if !complete {
        if let Ok(mut peer) = state.lock() {
            peer.outstanding = outstanding.len();
        }
        return Ok(());
    }
    let mut assembly = assemblies
        .remove(&piece)
        .ok_or_else(|| anyhow::anyhow!("completed v2 piece assembly disappeared"))?;
    let content = if let Some(data) = assembly.data.take() {
        Bytes::from(data)
    } else {
        let path = region.path.resolve(&context.save_root);
        scheduled_read_owned(
            &context.storage,
            IoClass::PeerRead,
            &path,
            region.file_offset,
            region.length as usize,
        )
        .await?
        .into_bytes()
    };
    let file = context
        .meta
        .files
        .iter()
        .find(|file| file.index == region.file_index)
        .ok_or_else(|| anyhow::anyhow!("v2 file {} not found", region.file_index))?;
    let expected_hash = expected_v2_piece_hash(&context.meta, file, piece)?;
    let actual_hash =
        actual_v2_piece_hash(content.as_ref(), file.length, context.meta.piece_length);
    if Some(actual_hash) != expected_hash {
        anyhow::bail!("v2 piece {piece} merkle hash mismatch");
    }
    if content.len() as u32 != region.length {
        anyhow::bail!("v2 piece read length does not match map");
    }
    if in_memory {
        let path = region.path.resolve(&context.save_root);
        for (index, chunk) in content
            .chunks(rt_peer_wire::message::MAX_BLOCK_SIZE as usize)
            .enumerate()
        {
            let begin = u64::try_from(index)
                .ok()
                .and_then(|index| {
                    index.checked_mul(u64::from(rt_peer_wire::message::MAX_BLOCK_SIZE))
                })
                .ok_or_else(|| anyhow::anyhow!("v2 piece write offset overflow"))?;
            scheduled_write(
                &context.storage,
                IoClass::PeerWrite,
                &path,
                region.file_offset.saturating_add(begin),
                Bytes::copy_from_slice(chunk),
                true,
            )
            .await?;
        }
    }
    if context.local_have.write().await.get(piece as usize) {
        return Ok(());
    }
    context.local_have.write().await.set(piece as usize, true);
    framed.send(Message::Have(piece)).await?;
    let _ = context
        .events
        .send(V2PeerEvent::Downloaded {
            peer: context.peer,
            bytes: u64::from(region.length),
        })
        .await;
    if let Ok(mut peer) = state.lock() {
        peer.record_transfer(false, u64::from(region.length));
        peer.outstanding = outstanding.len();
        peer.assembly_buffers = assemblies.len() as u64;
        peer.assembly_bytes = assemblies
            .values()
            .filter_map(|assembly| assembly.data.as_ref().map(|data| data.len() as u64))
            .sum();
    }
    Ok(())
}

async fn serve_v2_request(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    state: &Arc<Mutex<V2PeerState>>,
    piece: u32,
    begin: u32,
    length: u32,
) -> anyhow::Result<()> {
    let region = match context.piece_map.validate_request(piece, begin, length) {
        Ok(region) => region,
        Err(_) => {
            framed
                .send(Message::Reject {
                    piece,
                    begin,
                    length,
                })
                .await?;
            return Ok(());
        }
    };
    if state.lock().map(|peer| peer.upload_choked).unwrap_or(true) {
        framed
            .send(Message::Reject {
                piece,
                begin,
                length,
            })
            .await?;
        return Ok(());
    }
    if region.pad {
        // BEP 47 permits an implementation to omit padding files locally,
        // but it still has to answer a legacy peer that requests their
        // synthetic zero bytes.
        let data = Bytes::from(vec![0u8; region.length as usize]);
        context
            .network_budget
            .upload()
            .acquire(u64::from(region.length))
            .await;
        framed.send(Message::Piece { piece, begin, data }).await?;
        return Ok(());
    }
    if !context.local_have.read().await.get(piece as usize) {
        framed
            .send(Message::Reject {
                piece,
                begin,
                length,
            })
            .await?;
        return Ok(());
    }
    let path = region.path.resolve(&context.save_root);
    let data = match scheduled_read_owned(
        &context.storage,
        IoClass::PeerRead,
        &path,
        region.file_offset,
        region.length as usize,
    )
    .await
    {
        Ok(data) => data.into_bytes(),
        Err(error) => {
            debug!(
                component = "torrent_v2",
                operation = "read_upload_block",
                peer = %context.peer,
                piece,
                begin,
                length,
                result = "rejected",
                error = %error,
                "unable to read requested pure-v2 block"
            );
            framed
                .send(Message::Reject {
                    piece,
                    begin,
                    length,
                })
                .await?;
            return Ok(());
        }
    };
    context
        .network_budget
        .upload()
        .acquire(u64::from(region.length))
        .await;
    framed
        .send(Message::Piece {
            piece,
            begin,
            data: data.clone(),
        })
        .await?;
    if let Ok(mut peer) = state.lock() {
        peer.record_transfer(true, u64::from(region.length));
    }
    let _ = context
        .events
        .send(V2PeerEvent::Uploaded {
            peer: context.peer,
            bytes: u64::from(region.length),
        })
        .await;
    Ok(())
}

async fn serve_v2_hash_request(
    context: &V2PeerContext,
    framed: &mut PeerIo,
    pieces_root: [u8; 32],
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
) -> anyhow::Result<()> {
    let reject = || Message::HashReject {
        pieces_root,
        base_layer,
        index,
        length,
        proof_layers,
    };
    let Some(file) = context
        .meta
        .files
        .iter()
        .find(|file| file.pieces_root == Some(pieces_root))
    else {
        framed.send(reject()).await?;
        return Ok(());
    };
    if file.length > V2_MAX_HASH_EXCHANGE_FILE_BYTES {
        framed.send(reject()).await?;
        return Ok(());
    }
    let leaf_count = file.length.div_ceil(V2_BLOCK_SIZE as u64).max(1);
    let Some(_) = hash_request_bounds(
        usize::try_from(leaf_count).unwrap_or(usize::MAX),
        base_layer,
        index,
        length,
        proof_layers,
    ) else {
        framed.send(reject()).await?;
        return Ok(());
    };
    if !file.pad && !hash_request_file_is_available(context, file, base_layer, index, length).await
    {
        framed.send(reject()).await?;
        return Ok(());
    }
    let hash_memory = leaf_count
        .saturating_mul(4)
        .saturating_mul(std::mem::size_of::<[u8; 32]>() as u64);
    let Some(_hash_memory_lease) = context.resources.try_acquire(
        MemoryClass::PeerBuffer,
        file.length.saturating_add(hash_memory),
    ) else {
        framed.send(reject()).await?;
        return Ok(());
    };
    let leaves = if file.pad {
        // BEP 47 padding is synthetic zero content. Its hash tree is still
        // part of the v2 file metadata when a creator included the padding
        // leaf, so serve it without requiring a local `.pad/*` file.
        v2_zero_leaf_hashes(file.length)?
    } else {
        let data = match read_v2_file(context, file).await {
            Ok(data) => data,
            Err(error) => {
                debug!(
                    component = "torrent_v2",
                    operation = "read_hash_exchange_file",
                    peer = %context.peer,
                    file = ?file.path,
                    result = "rejected",
                    error = %error,
                    "unable to read v2 hash-exchange data"
                );
                framed.send(reject()).await?;
                return Ok(());
            }
        };
        v2_leaf_hashes(&data)
    };
    let layers = merkle_layers(&leaves);
    let Some(hashes) = hashes_for_v2_request(
        &layers,
        leaves.len(),
        base_layer,
        index,
        length,
        proof_layers,
    ) else {
        framed.send(reject()).await?;
        return Ok(());
    };
    framed
        .send(Message::Hashes {
            pieces_root,
            base_layer,
            index,
            length,
            proof_layers,
            hashes,
        })
        .await?;
    Ok(())
}

/// A hash request is useful only when all content pieces covered by its base
/// range have been announced. Serving a hash block from an unverified local
/// file would let a peer authenticate data that this client must not claim to
/// possess. Padding-only ranges need no local file because their hashes are
/// the protocol-defined zero padding nodes.
async fn hash_request_file_is_available(
    context: &V2PeerContext,
    file: &TorrentFileV2,
    base_layer: u32,
    index: u32,
    length: u32,
) -> bool {
    let Some(hash_span) = (V2_BLOCK_SIZE as u64).checked_shl(base_layer) else {
        return false;
    };
    let Some(start) = u64::from(index).checked_mul(hash_span) else {
        return false;
    };
    let Some(end) = start.checked_add(u64::from(length).saturating_mul(hash_span)) else {
        return false;
    };
    let content_end = end.min(file.length);
    if start >= content_end {
        return true;
    }
    let Some(first_offset) = file.piece_offset.checked_add(start) else {
        return false;
    };
    let Some(last_offset) = file.piece_offset.checked_add(content_end.saturating_sub(1)) else {
        return false;
    };
    if context.piece_map.piece_length == 0 {
        return false;
    }
    let first_piece = first_offset / context.piece_map.piece_length;
    let last_piece = last_offset / context.piece_map.piece_length;
    let have = context.local_have.read().await;
    (first_piece..=last_piece).all(|piece| {
        usize::try_from(piece)
            .ok()
            .is_some_and(|piece| have.get(piece))
    })
}

/// Build the `hashes` payload for a BEP 52 request. The requested base-layer
/// hashes are followed by the uncle hashes needed to reach the file root.
/// The first `log2(length) - 1` proof layers are implicit because the
/// requested power-of-two range already contains those complete child layers;
/// they remain counted in `proof_layers` as required by the wire format.
fn hashes_for_v2_request(
    layers: &[Vec<[u8; 32]>],
    leaf_count: usize,
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
) -> Option<Vec<[u8; 32]>> {
    let (base, start, count, proof_count) =
        hash_request_bounds(leaf_count, base_layer, index, length, proof_layers)?;
    let layer = layers.get(base)?;
    let end = start.checked_add(count)?;
    if end > layer.len() {
        return None;
    }
    let padded_leaf_count = layers.first()?.len();
    let expected_layer_len = padded_leaf_count.checked_shr(base_layer)?;
    if expected_layer_len == 0 || layer.len() != expected_layer_len {
        return None;
    }

    let mut hashes = layer[start..end].to_vec();
    let omitted_layers = length.trailing_zeros() as usize;
    if omitted_layers <= proof_count {
        for proof_index in omitted_layers..=proof_count {
            let proof_layer = base.checked_add(proof_index)?;
            // `proof_layers` is the highest ancestor depth requested. The
            // root itself has no uncle, so hash_request_bounds excludes that
            // layer before this loop.
            let node_index = start.checked_shr(proof_index as u32)?;
            let sibling = node_index ^ 1;
            hashes.push(*layers.get(proof_layer)?.get(sibling)?);
        }
    }
    Some(hashes)
}

/// Validate a BEP 52 hash request against a file's padded Merkle-tree shape.
/// The returned proof-layer value is the inclusive ancestor depth used by the
/// wire format: for a two-hash request, `proof_layers = 2` asks for the two
/// uncles above the requested subtree when the root is at depth three.
fn hash_request_bounds(
    leaf_count: usize,
    base_layer: u32,
    index: u32,
    length: u32,
    proof_layers: u32,
) -> Option<(usize, usize, usize, usize)> {
    if leaf_count == 0 || length < 2 || !length.is_power_of_two() || !index.is_multiple_of(length) {
        return None;
    }
    if length > V2_MAX_HASH_REQUEST_LENGTH {
        return None;
    }
    let padded_leaf_count = leaf_count.checked_next_power_of_two()?;
    let tree_height = padded_leaf_count.trailing_zeros() as usize;
    let base = usize::try_from(base_layer).ok()?;
    if base > tree_height {
        return None;
    }
    let layer_len = padded_leaf_count.checked_shr(base_layer)?;
    let start = usize::try_from(index).ok()?;
    let count = usize::try_from(length).ok()?;
    let end = start.checked_add(count)?;
    if layer_len < count || end > layer_len {
        return None;
    }
    let proof_count = usize::try_from(proof_layers).ok()?;
    let max_proof_layer = tree_height.saturating_sub(base.saturating_add(1));
    if proof_count > max_proof_layer {
        return None;
    }
    Some((base, start, count, proof_count))
}

async fn read_v2_file(context: &V2PeerContext, file: &TorrentFileV2) -> anyhow::Result<Bytes> {
    let path = file.path.resolve(&context.save_root);
    let mut data = Vec::new();
    data.try_reserve_exact(file.length as usize)
        .map_err(|error| anyhow::anyhow!("v2 hash exchange allocation failed: {error}"))?;
    let mut offset = 0u64;
    while offset < file.length {
        let length = (file.length - offset).min(V2_BLOCK_SIZE as u64) as usize;
        let read = scheduled_read_owned(&context.storage, IoClass::PeerRead, &path, offset, length)
            .await?;
        data.extend_from_slice(read.as_slice());
        offset += length as u64;
    }
    Ok(Bytes::from(data))
}

fn v2_leaf_hashes(data: &[u8]) -> Vec<[u8; 32]> {
    if data.is_empty() {
        return vec![BlockHash::of(&[]).0];
    }
    data.chunks(V2_BLOCK_SIZE)
        .map(|chunk| BlockHash::of(chunk).0)
        .collect()
}

fn v2_zero_leaf_hashes(length: u64) -> anyhow::Result<Vec<[u8; 32]>> {
    let leaf_count = usize::try_from(length.div_ceil(V2_BLOCK_SIZE as u64).max(1))
        .map_err(|error| anyhow::anyhow!("v2 padding leaf count does not fit usize: {error}"))?;
    let mut leaves = Vec::new();
    leaves
        .try_reserve_exact(leaf_count)
        .map_err(|error| anyhow::anyhow!("v2 padding hash allocation failed: {error}"))?;
    if length == 0 {
        leaves.push(BlockHash::of(&[]).0);
        return Ok(leaves);
    }
    let full_zero_block = vec![0u8; V2_BLOCK_SIZE];
    let full_hash = BlockHash::of(&full_zero_block).0;
    leaves.resize(leaf_count, full_hash);
    let remainder = (length % V2_BLOCK_SIZE as u64) as usize;
    if remainder != 0 {
        leaves[leaf_count - 1] = BlockHash::of(&full_zero_block[..remainder]).0;
    }
    Ok(leaves)
}

fn actual_v2_piece_hash(data: &[u8], file_length: u64, piece_length: u64) -> [u8; 32] {
    if file_length <= piece_length {
        merkle_root(&v2_leaf_hashes(data))
    } else {
        piece_layer_hash(data, piece_length as usize).unwrap_or([0u8; 32])
    }
}

fn expected_v2_piece_hash(
    meta: &TorrentMetaV2,
    file: &TorrentFileV2,
    piece: u32,
) -> anyhow::Result<Option<[u8; 32]>> {
    if file.length <= meta.piece_length {
        return Ok(file.pieces_root);
    }
    let root = file
        .pieces_root
        .ok_or_else(|| anyhow::anyhow!("v2 layered file has no pieces root"))?;
    let first_piece = file.piece_offset / meta.piece_length;
    let index = piece
        .checked_sub(
            u32::try_from(first_piece).map_err(|_| anyhow::anyhow!("v2 piece offset overflow"))?,
        )
        .ok_or_else(|| anyhow::anyhow!("v2 piece precedes its file"))? as usize;
    Ok(meta
        .piece_layers
        .get(&root)
        .and_then(|hashes| hashes.get(index))
        .copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_roundtrip_preserves_v2_shape() {
        let mut bitmap = V2Bitmap::new(10);
        bitmap.set(0, true);
        bitmap.set(9, true);
        let encoded = bitmap.to_bitfield();
        assert_eq!(
            V2Bitmap::from_bitfield(&encoded, 10).unwrap().count_ones(),
            2
        );
    }

    #[test]
    fn metainfo_tracker_tiers_are_not_flattened() {
        let meta = TorrentMetaV2 {
            info_hash_v2: [0; 32],
            announce: Some("https://primary.example/announce".to_owned()),
            announce_list: vec![
                vec![
                    "https://primary.example/announce".to_owned(),
                    "https://backup-a.example/announce".to_owned(),
                ],
                vec![
                    "https://backup-b.example/announce".to_owned(),
                    "https://backup-a.example/announce".to_owned(),
                ],
            ],
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "tiered".to_owned(),
            piece_length: 16 * 1024,
            files: Vec::new(),
            piece_layers: HashMap::new(),
            private: false,
            raw: Vec::new(),
        };

        let tiers = tracker_tiers_from_meta_v2(&meta);
        let urls = tiers
            .iter()
            .map(|tier| {
                tier.iter()
                    .map(|tracker| tracker.url.as_str())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                vec![
                    "https://primary.example/announce",
                    "https://backup-a.example/announce"
                ],
                vec!["https://backup-b.example/announce"],
            ]
        );
    }

    #[test]
    fn piece_assembly_accepts_the_full_peer_coordinate_range() {
        let resources = ResourceGovernor::new(Default::default());
        let block_count = 65_537usize;
        let region_len = u32::try_from(
            block_count
                .saturating_mul(rt_peer_wire::message::MAX_BLOCK_SIZE as usize)
                .saturating_sub(1),
        )
        .unwrap();
        let assembly = V2PieceAssembly::new(0, region_len, 0, &resources).unwrap();
        assert_eq!(assembly.received.len(), block_count);
    }

    #[test]
    fn short_piece_uses_file_root_tree_not_piece_layer_padding() {
        let data = vec![7u8; V2_BLOCK_SIZE + 3];
        let actual = actual_v2_piece_hash(&data, data.len() as u64, 64 * 1024);
        assert_eq!(actual, merkle_root(&v2_leaf_hashes(&data)));
        assert_ne!(actual, piece_layer_hash(&data, 64 * 1024).unwrap());
    }

    #[test]
    fn hash_exchange_includes_only_required_uncles() {
        let leaves = (0..8)
            .map(|value| BlockHash::of(&[value]).0)
            .collect::<Vec<_>>();
        let layers = merkle_layers(&leaves);
        let hashes = hashes_for_v2_request(&layers, leaves.len(), 0, 0, 2, 2).unwrap();

        assert_eq!(hashes.len(), 4);
        assert_eq!(&hashes[..2], &layers[0][..2]);
        assert_eq!(hashes[2], layers[1][1]);
        assert_eq!(hashes[3], layers[2][1]);
        assert!(hashes_for_v2_request(&layers, leaves.len(), 0, 0, 2, 3).is_none());
    }

    #[test]
    fn hash_exchange_uses_zero_padding_at_file_end() {
        let leaves = (0..3)
            .map(|value| BlockHash::of(&[value]).0)
            .collect::<Vec<_>>();
        let layers = merkle_layers(&leaves);
        let hashes = hashes_for_v2_request(&layers, leaves.len(), 0, 2, 2, 1).unwrap();

        assert_eq!(hashes[0], layers[0][2]);
        assert_eq!(hashes[1], [0; 32]);
    }

    #[test]
    fn padding_hash_tree_uses_zero_content_without_a_file() {
        let leaves = v2_zero_leaf_hashes((V2_BLOCK_SIZE + 3) as u64).unwrap();
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0], BlockHash::of(&vec![0u8; V2_BLOCK_SIZE]).0);
        assert_eq!(leaves[1], BlockHash::of(&[0u8; 3]).0);
    }

    #[test]
    fn hash_exchange_allows_bounded_padding_at_file_end() {
        let leaves = (0..3)
            .map(|value| BlockHash::of(&[value]).0)
            .collect::<Vec<_>>();
        let layers = merkle_layers(&leaves);

        assert!(hashes_for_v2_request(&layers, leaves.len(), 0, 2, 2, 0).is_some());
        assert!(hashes_for_v2_request(&layers, leaves.len(), 0, 0, 2, 0).is_some());
    }

    #[test]
    fn hash_exchange_rejects_invalid_tree_coordinates_before_reading() {
        assert!(hash_request_bounds(8, 0, 0, 1, 0).is_none());
        assert!(hash_request_bounds(8, 0, 1, 2, 0).is_none());
        assert!(hash_request_bounds(8, 0, 0, 4, 2).is_some());
        assert!(hash_request_bounds(8, 0, 0, 4, 3).is_none());
        assert!(hash_request_bounds(8, 0, 4, 8, 0).is_none());
        assert!(hash_request_bounds(8, 4, 0, 2, 0).is_none());
        assert!(hash_request_bounds(1024, 0, 0, 1024, 0).is_none());
    }
}
