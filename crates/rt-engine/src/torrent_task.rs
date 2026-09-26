/// Per-torrent async task.
///
/// One tokio task per torrent owns: tracker announce loop, peer connection
/// management, piece picker, and storage writes.
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use rt_bencode::{BValue, Decoder};
use rusqlite::Connection;
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, RwLock};
use tokio::time::{interval, sleep, timeout, Instant as TokioInstant, Sleep};
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};
use url::Url;

use std::sync::Arc;

use rt_fastresume::{
    FastresumeState, FastresumeStore, FileHint, ImportPolicy, PartialPieceState, PieceState,
    MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE,
};
#[cfg(test)]
use rt_metainfo::{torrent_info_bytes, TorrentMeta};
use rt_metainfo::{torrent_info_bytes_with_allocation_reservation, TorrentFileV1, TorrentMetaV1};
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
use rt_session::{SessionRegistry, TorrentEntry, TorrentState};
use rt_storage::{
    scheduler::{scheduled_read_owned, scheduled_write},
    IoClass, MountScheduler, PieceVerifier, SchedulerConfig, StorageIoConfig, VerifyResult,
};
#[cfg(test)]
use rt_tracker::{AnnounceResponse, TrackerError};
use rt_tracker::{
    TrackerEvent, TrackerState, TrackerStatus, MAX_TRACKER_PEERS, MAX_TRACKER_STATE_ID_BYTES,
    MAX_TRACKER_STATE_TEXT_BYTES,
};
use rt_utp::UtpStream;

#[path = "torrent_task/peer_connections.rs"]
mod peer_connections;
#[path = "torrent_task/peer_session.rs"]
mod peer_session;
#[path = "torrent_task/peer_transfer.rs"]
mod peer_transfer;

use crate::db_worker::DbExecutor;
use crate::egress_policy::{OutboundEgressPolicy, OutboundTargetKind};
use crate::network_budget::{GlobalNetworkBudget, RateLimitCancellation, SharedRateLimiter};
use crate::tracker_runtime::{
    announce_tracker, bounded_response_body, first_tracker_key, next_tracker_key, protocol_numwant,
    tracker_keys_for_announce, tracker_keys_for_stopped_announce, url_log_target,
    TrackerAnnounceContext, TrackerAnnounceResult, TrackerAnnounceSpec, TrackerKey, TrackerWorkers,
    MAX_TRACKER_ANNOUNCES_IN_FLIGHT, STOPPED_TRACKER_ANNOUNCE_DEADLINE,
};
use crate::{EnginePeerSnapshot, EngineTorrentLimits, EngineWebseedSnapshot, TorrentRuntimeStats};

const LOCAL_UT_METADATA_ID: u8 = 1;
const LOCAL_UT_PEX_ID: u8 = 2;
const METADATA_PIECE_SIZE: usize = 16 * 1024;
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

fn lock_prepared_files(files: &Mutex<HashSet<u32>>) -> MutexGuard<'_, HashSet<u32>> {
    match files.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // Prepared-file membership is an optimization. Re-preparing is
            // safe: storage refuses to shrink existing payloads and length
            // setup is idempotent, so discard potentially stale membership.
            let mut guard = poisoned.into_inner();
            guard.clear();
            files.clear_poison();
            guard
        }
    }
}
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
// Terminal and seed-limit transitions must leave time for persistence and
// peer teardown. Tracker stop delivery is best effort and cannot consume the
// engine's entire command/shutdown budget when a tracker is unresponsive.
const STOPPED_TRACKER_CONTROL_ANNOUNCE_DEADLINE: Duration = Duration::from_secs(1);
const WEBSEED_RETRY_BASE: Duration = Duration::from_secs(1);
const WEBSEED_RETRY_MAX: Duration = Duration::from_secs(300);
const MAX_UT_PEX_PEERS: usize = 2_048;
// BEP 11 payloads arrive from untrusted peers inside a legal peer-wire
// message. Keep a flat bencode node bomb from reaching the parser's much
// larger generic default before the compact-peer count checks run.
const MAX_UT_PEX_BENCODE_NODES: usize = 16 * 1024;
// Peer snapshots are compatibility/API projections rather than a scheduling
// primitive. Keep a damaged or unusually configured torrent from allocating
// an unbounded response vector when an endpoint asks for its peers.
pub(crate) const MAX_PEER_SNAPSHOT_ITEMS: usize = 16_384;
const DOWNLOAD_BUCKET_MIN_CAPACITY: u64 = MAX_BLOCK_SIZE as u64;

fn peer_event_channel_capacity(max_peers: usize) -> usize {
    max_peers.clamp(64, 512)
}

fn download_bucket_capacity(limit: u64) -> u64 {
    limit.max(DOWNLOAD_BUCKET_MIN_CAPACITY)
}

fn consume_download_tokens(tokens: &mut u64, limit: u64, bytes: u64) -> bool {
    if *tokens < bytes {
        return false;
    }
    *tokens -= bytes;
    *tokens = (*tokens).min(download_bucket_capacity(limit));
    true
}

fn db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn build_piece_map(piece_length: u64, files: &[TorrentFileV1]) -> Result<Arc<PieceMap>, String> {
    PieceMap::new_with_padding(
        piece_length,
        files
            .iter()
            .map(|file| FileSpan {
                file_index: file.index,
                path: file.path.clone(),
                content_offset: file.offset,
                length: file.length,
            })
            .collect(),
        files
            .iter()
            .filter_map(|file| file.pad.then_some(file.index)),
    )
    .map(Arc::new)
    .map_err(|error| format!("building torrent piece map: {error}"))
}

fn validate_padding_block(
    piece_map: &PieceMap,
    piece: u32,
    begin: u32,
    data: &[u8],
) -> anyhow::Result<()> {
    let length = u32::try_from(data.len())
        .map_err(|_| anyhow::anyhow!("peer block length does not fit in u32"))?;
    let regions = piece_map.validate_request(piece, begin, length)?;
    let mut data_offset = 0usize;
    for region in regions {
        let region_len = usize::try_from(region.length)
            .map_err(|_| anyhow::anyhow!("piece region length does not fit in memory"))?;
        let end = data_offset
            .checked_add(region_len)
            .ok_or_else(|| anyhow::anyhow!("piece region data offset overflow"))?;
        let region_data = data
            .get(data_offset..end)
            .ok_or_else(|| anyhow::anyhow!("peer block does not cover mapped piece regions"))?;
        if region.pad && region_data.iter().any(|byte| *byte != 0) {
            anyhow::bail!("peer block contains non-zero bytes for BEP 47 padding");
        }
        data_offset = end;
    }
    if data_offset != data.len() {
        anyhow::bail!("mapped piece regions do not cover the peer block");
    }
    Ok(())
}

const TORRENT_METADATA_MEMORY_BASE: usize = 64 * 1024;

fn add_owned_string_bytes(total: &mut usize, value: &String) {
    *total = total.saturating_add(value.capacity());
}

fn add_safe_path_bytes(total: &mut usize, path: &rt_path::SafeRelPath) {
    // SafeRelPath's component vector is private, but construction bounds its
    // capacity to the component count. Charge the vector and every owned
    // String rather than just the joined display length.
    *total = total.saturating_add(
        path.components()
            .len()
            .saturating_mul(std::mem::size_of::<String>()),
    );
    for component in path.components() {
        add_owned_string_bytes(total, component);
    }
}

fn add_v1_file_graph_bytes(
    total: &mut usize,
    files: &[TorrentFileV1],
    capacity: usize,
    entry_size: usize,
) {
    *total = total.saturating_add(capacity.saturating_mul(entry_size));
    for file in files {
        add_safe_path_bytes(total, &file.path);
    }
}

fn persistent_file_graph_memory_bytes(files: &[TorrentFileV1]) -> usize {
    let mut total = 0;
    add_v1_file_graph_bytes(
        &mut total,
        files,
        files.len(),
        std::mem::size_of::<TorrentFileV1>(),
    );
    add_v1_file_graph_bytes(
        &mut total,
        files,
        files.len(),
        std::mem::size_of::<FileSpan>(),
    );
    total
}

/// Estimate the allocations retained by one live v1 task outside the piece
/// hash/picker lease and the exact BEP9 upload-payload lease.
///
/// The task owns the parsed file graph, a second copy used to rebuild durable
/// file policy, and a third path graph inside PieceMap. It also materializes
/// tracker state and webseed bookkeeping from the parsed metadata. Admission
/// must reserve this lifetime memory before the task is constructed; otherwise
/// a session containing many large multi-file torrents can bypass the metadata
/// governor even though every individual parser input is bounded. Tracker
/// state is reserved separately after the durable tracker override is known.
pub(crate) fn persistent_torrent_metadata_memory_bytes(meta: &TorrentMetaV1) -> usize {
    let mut total = TORRENT_METADATA_MEMORY_BASE
        .saturating_add(std::mem::size_of::<TorrentMetaV1>())
        .saturating_add(std::mem::size_of::<PieceMap>())
        .saturating_add(2 * std::mem::size_of::<usize>());

    add_v1_file_graph_bytes(
        &mut total,
        &meta.files,
        meta.files.capacity(),
        std::mem::size_of::<TorrentFileV1>(),
    );
    // `metainfo_files` is a deep clone used to restore the original paths
    // after a durable file-policy projection changes them.
    total = total.saturating_add(persistent_file_graph_memory_bytes(&meta.files));

    add_owned_string_bytes(&mut total, &meta.name);
    if let Some(comment) = &meta.comment {
        add_owned_string_bytes(&mut total, comment);
    }
    if let Some(created_by) = &meta.created_by {
        add_owned_string_bytes(&mut total, created_by);
    }
    if let Some(announce) = &meta.announce {
        add_owned_string_bytes(&mut total, announce);
    }
    total = total.saturating_add(
        meta.announce_list
            .capacity()
            .saturating_mul(std::mem::size_of::<Vec<String>>()),
    );
    // tracker_tiers has one outer Vec and one inner Vec per non-empty source
    // tier, with a possible extra tier for the standalone announce URL.
    total = total.saturating_add(
        meta.announce_list
            .len()
            .saturating_add(1)
            .saturating_mul(std::mem::size_of::<Vec<TrackerState>>()),
    );
    for tier in &meta.announce_list {
        total = total.saturating_add(
            tier.capacity()
                .saturating_mul(std::mem::size_of::<String>()),
        );
        for tracker in tier {
            add_owned_string_bytes(&mut total, tracker);
        }
    }

    total = total.saturating_add(
        meta.webseeds
            .capacity()
            .saturating_mul(std::mem::size_of::<String>()),
    );
    for webseed in &meta.webseeds {
        add_owned_string_bytes(&mut total, webseed);
    }
    let webseed_count = meta.webseeds.len();
    total = total
        .saturating_add(webseed_count.saturating_mul(std::mem::size_of::<u8>()))
        .saturating_add(webseed_count.saturating_mul(std::mem::size_of::<Option<Instant>>()))
        .saturating_add(webseed_count.saturating_mul(std::mem::size_of::<i64>()))
        .saturating_add(webseed_count.saturating_mul(std::mem::size_of::<Option<Instant>>()));

    total
}

/// Estimate the complete retained tracker projection, including bounded
/// response fields that are not present in metainfo. The ID and status text
/// portions are reserved at their protocol bounds so a later announce cannot
/// grow a live task outside the governor after admission.
fn tracker_state_memory_bytes(tiers: &[Vec<TrackerState>], outer_capacity: usize) -> usize {
    let mut total = outer_capacity.saturating_mul(std::mem::size_of::<Vec<TrackerState>>());
    for tier in tiers {
        total = total.saturating_add(
            tier.capacity()
                .saturating_mul(std::mem::size_of::<TrackerState>()),
        );
        for tracker in tier {
            total = total
                .saturating_add(tracker.url.capacity())
                .saturating_add(MAX_TRACKER_STATE_ID_BYTES)
                .saturating_add(MAX_TRACKER_STATE_TEXT_BYTES);
        }
    }
    total
}

fn reserve_tracker_state_memory(
    resources: &ResourceGovernor,
    tiers: &[Vec<TrackerState>],
    outer_capacity: usize,
) -> Result<Vec<MemoryLease>, String> {
    let bytes = u64::try_from(tracker_state_memory_bytes(tiers, outer_capacity))
        .map_err(|_| "tracker state memory estimate does not fit in u64".to_owned())?;
    if bytes == 0 {
        return Ok(Vec::new());
    }
    resources
        .try_acquire(MemoryClass::Metadata, bytes)
        .map(|lease| vec![lease])
        .ok_or_else(|| format!("tracker state allocation of {bytes} bytes denied"))
}

fn parse_persisted_file_path(path: &str) -> Result<rt_path::SafeRelPath, String> {
    rt_path::SafeRelPath::from_slash_separated(path, cfg!(windows))
        .map_err(|error| format!("invalid persisted torrent file path {path:?}: {error}"))
}

fn restore_runtime_projection(entry: &mut TorrentEntry, previous: &TorrentEntry) {
    entry.total_length = previous.total_length;
    entry.amount_left = previous.amount_left;
    entry.state = previous.state;
    entry.completed_at = previous.completed_at;
    entry.error_message = previous.error_message.clone();
}

fn restore_progress_projection(entry: &mut TorrentEntry, previous: &TorrentEntry) {
    entry.total_length = previous.total_length;
    entry.amount_left = previous.amount_left;
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
    fn memory_bytes_for_len(len: usize) -> u64 {
        len.div_ceil(64).saturating_mul(std::mem::size_of::<u64>()) as u64
    }

    fn new(len: usize) -> Self {
        Self {
            len,
            words: vec![0; len.div_ceil(64)],
        }
    }

    /// A bitmap with every piece in `0..len` set. Used for BEP 6
    /// `HaveAll`, which stands in for a full `Bitfield` on the wire.
    fn all_true(len: usize) -> Self {
        Self {
            len,
            words: vec![u64::MAX; len.div_ceil(64)],
        }
    }

    #[cfg(test)]
    fn from_bools(bits: &[bool]) -> Self {
        let mut bitmap = Self::new(bits.len());
        for (index, value) in bits.iter().copied().enumerate() {
            if value {
                bitmap.set(index, true);
            }
        }
        bitmap
    }

    #[cfg(test)]
    fn from_bitfield(bits: &[u8], piece_count: usize) -> anyhow::Result<Self> {
        validate_bitfield_shape(bits, piece_count)?;
        Ok(Self::from_validated_bitfield(bits, piece_count))
    }

    fn from_validated_bitfield(bits: &[u8], piece_count: usize) -> Self {
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
        bitmap
    }

    fn memory_bytes(&self) -> u64 {
        Self::memory_bytes_for_len(self.len)
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
    /// Pause the task.  Callers that need a durable lifecycle acknowledgement
    /// provide a reply sender; internal best-effort control paths may leave it
    /// absent.
    Pause {
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    /// Begin resuming the task.  The acknowledgement is sent after the
    /// `Checking` transition is durable; the potentially long piece recheck
    /// continues afterward.
    Resume {
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
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
    /// Replace the persisted tracker override and apply it to the live task.
    /// A reply is used for API mutations; internal task-control paths may
    /// leave it absent.
    UpdateTrackers {
        trackers: Vec<String>,
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
    /// torrent was already paused before this call, so the caller can restore
    /// that state afterward instead of unconditionally resuming. The
    /// acknowledgement fails if the durable `Paused` transition could not be
    /// persisted.
    QuiesceForStorageMove {
        reply: oneshot::Sender<Result<bool, String>>,
    },
    /// TNG-002: re-points this task's cached `save_root` (and rebuilds the
    /// `MountScheduler` bound to it, so device-topology detection and any
    /// per-path handle-cache state is re-derived for the new location
    /// rather than staying pinned to the pre-move mount) after a storage
    /// move committed, then resumes activity unless the torrent was
    /// already paused before the move began. `new_save_root` is `None`
    /// when the move failed and rolled back -- the task simply resumes
    /// unchanged in that case). The reply is sent after the new runtime root
    /// is installed and, for an active torrent, the durable `Checking`
    /// transition succeeds. The potentially long recheck continues after the
    /// acknowledgement.
    ResumeAfterStorageMove {
        new_save_root: Option<PathBuf>,
        resume_paused: bool,
        reply: oneshot::Sender<Result<(), String>>,
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
    /// Re-check the shared ban policy and evict every matching active peer.
    /// The command is intentionally payload-free so one ban request does not
    /// multiply by the number of newly banned endpoints and torrent tasks.
    EvictBannedPeers,
    /// Legacy queue-filler command retained for DHT tests/compatibility. API
    /// callers must use the bounded result-bearing command below.
    GetPeers {
        reply: oneshot::Sender<Vec<EnginePeerSnapshot>>,
    },
    GetPeerSnapshotCount {
        reply: oneshot::Sender<usize>,
    },
    GetPeerSnapshots {
        max_entries: usize,
        reply: oneshot::Sender<Result<Vec<EnginePeerSnapshot>, String>>,
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
        stream: Box<UtpStream>,
        peer_addr: SocketAddr,
        handshake: Handshake,
        peer_permit: OwnedSemaphorePermit,
    },
}

async fn receive_torrent_command(
    cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
    pending_command: &mut Option<TorrentCmd>,
) -> Option<TorrentCmd> {
    if pending_command.is_some() {
        pending_command.take()
    } else {
        cmd_rx.recv().await
    }
}

/// Run the best-effort stopped announce without stranding the actor. A
/// lifecycle command can arrive after its caller has already received a
/// durable acknowledgement; keep that command in the actor's normal FIFO
/// path while the network future is cancelled.
async fn await_stopped_announce_or_queue_command(
    task: &mut TorrentTask,
    cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
    pending_command: &mut Option<TorrentCmd>,
) {
    // Deciding which branch won, and restoring `task.stopped_announced`, must
    // happen after `operation` (which mutably borrows `task`) is dropped —
    // doing the restore assignment inside the `select!` arm itself conflicts
    // with that live borrow under NLL. So the arms only report an outcome;
    // `task` is touched once, below, outside the pinned future's scope.
    enum Outcome {
        Completed,
        Interrupted(Option<TorrentCmd>),
    }

    let outcome = {
        let operation = task.announce_stopped();
        tokio::pin!(operation);
        tokio::select! {
            _ = &mut operation => Outcome::Completed,
            command = cmd_rx.recv() => Outcome::Interrupted(command),
        }
    };

    if let Outcome::Interrupted(command) = outcome {
        // `announce_stopped` consumes the one-shot flag before it starts its
        // network work. If a later lifecycle command, or a closed command
        // channel, interrupts that future, no stopped request may have
        // reached any tracker. Restore the flag so the shutdown path can
        // make one bounded retry, and leave any received command pending in
        // the actor's normal FIFO path.
        task.stopped_announced = false;
        if let Some(command) = command {
            *pending_command = Some(command);
        }
    }
}

fn reject_torrent_command_during_shutdown(command: TorrentCmd) {
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

fn reject_pending_torrent_commands(
    cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
    pending_command: &mut Option<TorrentCmd>,
) {
    if let Some(command) = pending_command.take() {
        reject_torrent_command_during_shutdown(command);
    }
    while let Ok(command) = cmd_rx.try_recv() {
        reject_torrent_command_during_shutdown(command);
    }
}

#[derive(Debug)]
enum RecheckOutcome {
    Complete,
    Paused {
        reply: Option<oneshot::Sender<Result<(), String>>>,
        previous_paused: bool,
        previous_restore_state: Option<TorrentState>,
        state_persisted: bool,
    },
    Cancelled,
    Shutdown,
    Failed(String),
}

struct RecheckInvalidPieces {
    values: Vec<i64>,
    total: usize,
}

impl RecheckInvalidPieces {
    fn new(piece_count: u32) -> Self {
        Self {
            values: Vec::with_capacity((piece_count as usize).min(rt_db::MAX_JOB_INVALID_PIECES)),
            total: 0,
        }
    }

    fn record(&mut self, piece: u32) {
        self.total = self.total.saturating_add(1);
        if self.values.len() < rt_db::MAX_JOB_INVALID_PIECES {
            self.values.push(piece as i64);
        }
    }
}

const JOB_STATE_RUNNING: &str = "running";
const JOB_STATE_PAUSED: &str = "paused";
const JOB_STATE_CANCELLING: &str = "cancelling";
const JOB_STATE_CANCELLED: &str = "cancelled";
const JOB_STATE_COMPLETED: &str = "completed";
const JOB_STATE_FAILED: &str = "failed";
const MAX_IN_MEMORY_PIECE_ASSEMBLIES: usize = 64;
const MAX_IN_MEMORY_PIECE_ASSEMBLY_BYTES_PER_TORRENT: usize = 64 * 1024 * 1024;
const PEER_REQUEST_PIPELINE_NORMAL: usize = 32;
const PEER_REQUEST_PIPELINE_CONSTRAINED: usize = 8;
const TRACKER_PEER_CACHE_MIN: usize = 256;
const TRACKER_PEER_CACHE_MULTIPLIER: usize = 4;
// A HashSet bucket contains the socket address plus control bytes and can
// round capacity up during allocation. Reserve conservatively for the full
// bounded cache before accepting any tracker peers so later inserts cannot
// grow this state outside the governor.
const TRACKER_PEER_CACHE_BYTES_PER_ENTRY: u64 = 128;
// A dirty-piece watermark is only needed between storage-sync barriers. Keep
// the live set bounded; if more pieces become dirty than this, an unclean
// fastresume save deliberately carries no watermark and therefore falls back
// to a full recheck after a crash.
const MAX_RUNTIME_DIRTY_PIECES: usize = 65_536;

#[derive(Debug, Default)]
struct DirtyPieceTracker {
    pieces: HashSet<u32>,
    overflowed: bool,
}

impl DirtyPieceTracker {
    fn insert(&mut self, piece: u32) {
        if self.overflowed || self.pieces.contains(&piece) {
            return;
        }
        if self.pieces.len() >= MAX_RUNTIME_DIRTY_PIECES {
            self.pieces.clear();
            self.overflowed = true;
            return;
        }
        self.pieces.insert(piece);
    }

    fn len(&self) -> u64 {
        if self.overflowed {
            (MAX_RUNTIME_DIRTY_PIECES as u64).saturating_add(1)
        } else {
            self.pieces.len() as u64
        }
    }

    fn is_overflowed(&self) -> bool {
        self.overflowed
    }

    fn clear(&mut self) {
        self.pieces.clear();
        self.overflowed = false;
    }
}

fn effective_piece_assembly_soft_cap(configured_bytes: usize) -> usize {
    configured_bytes.min(MAX_IN_MEMORY_PIECE_ASSEMBLY_BYTES_PER_TORRENT)
}

fn memory_aware_request_pipeline(piece_assembly_bytes: usize, soft_cap_bytes: usize) -> usize {
    if soft_cap_bytes == 0 {
        // A zero assembly cap disables aggregation, not downloading. Blocks
        // take the direct-write path in this mode, so keep the peer pipeline
        // open instead of permanently starving the torrent.
        return PEER_REQUEST_PIPELINE_NORMAL;
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

pub(crate) fn tracker_peer_cache_cap(max_peers: usize) -> usize {
    max_peers
        .saturating_mul(TRACKER_PEER_CACHE_MULTIPLIER)
        .clamp(TRACKER_PEER_CACHE_MIN, MAX_TRACKER_PEERS)
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

#[cfg(test)]
fn reserve_peer_bitfield_bytes(
    resources: &ResourceGovernor,
    pieces: &PieceBitmap,
) -> anyhow::Result<MemoryLease> {
    reserve_peer_bitfield_len(resources, pieces.len())
}

fn reserve_peer_bitfield_len(
    resources: &ResourceGovernor,
    piece_count: usize,
) -> anyhow::Result<MemoryLease> {
    let bytes = PieceBitmap::memory_bytes_for_len(piece_count);
    resources
        .try_acquire(MemoryClass::PeerBuffer, bytes)
        .ok_or_else(|| anyhow::anyhow!("peer bitfield allocation of {bytes} bytes denied"))
}

fn reserve_peer_bitfield_wire_bytes(
    resources: &ResourceGovernor,
    pieces: &PieceBitmap,
) -> anyhow::Result<MemoryLease> {
    let bitfield_bytes = u64::try_from(pieces.len().div_ceil(8))
        .map_err(|_| anyhow::anyhow!("peer bitfield wire allocation does not fit in u64"))?;
    // The payload is held by Message, Message::encode creates a second
    // length-prefixed copy, and TCP's framed writer can retain a third copy
    // until the send flushes. Reserve the peak rather than letting a large
    // torrent's piece count bypass the peer-buffer governor.
    let bytes = bitfield_bytes.saturating_mul(3).saturating_add(5);
    resources
        .try_acquire(MemoryClass::PeerBuffer, bytes)
        .ok_or_else(|| anyhow::anyhow!("peer bitfield wire allocation of {bytes} bytes denied"))
}

fn reserve_peer_event_bytes(
    resources: &ResourceGovernor,
    bytes: usize,
) -> anyhow::Result<MemoryLease> {
    let bytes = u64::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("peer event allocation does not fit in u64"))?;
    resources
        .try_acquire(MemoryClass::PeerBuffer, bytes)
        .ok_or_else(|| anyhow::anyhow!("peer event allocation of {bytes} bytes denied"))
}

fn reserve_metadata_payload_bytes(
    resources: &ResourceGovernor,
    bytes: usize,
) -> anyhow::Result<MemoryLease> {
    let bytes = u64::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("metadata payload allocation does not fit in u64"))?;
    resources
        .try_acquire(MemoryClass::Metadata, bytes)
        .ok_or_else(|| anyhow::anyhow!("metadata payload allocation of {bytes} bytes denied"))
}

fn prepare_metadata_payload(
    resources: &ResourceGovernor,
    info_hash: &str,
    raw: &[u8],
) -> (Option<Arc<Vec<u8>>>, Option<MemoryLease>) {
    let mut parse_memory_lease = match crate::engine::reserve_torrent_parse_memory(
        resources,
        raw.len(),
        "metadata upload extraction",
    ) {
        Ok(lease) => lease,
        Err(error) => {
            warn!(
                component = "memory",
                operation = "extract_upload_payload",
                torrent = %info_hash,
                result = "denied",
                error = %error,
                "disabling metadata upload because parser memory admission failed"
            );
            return (None, None);
        }
    };
    let mut reserve = |additional| {
        u64::try_from(additional)
            .map(|bytes| parse_memory_lease.try_grow(bytes))
            .unwrap_or(false)
    };
    match torrent_info_bytes_with_allocation_reservation(raw, &mut reserve) {
        Ok(metadata) => match reserve_metadata_payload_bytes(resources, metadata.len()) {
            Ok(lease) => (Some(Arc::new(metadata)), Some(lease)),
            Err(error) => {
                warn!(
                    component = "metadata",
                    operation = "reserve_upload_payload",
                    torrent = %info_hash,
                    result = "denied",
                    error = %error,
                    "disabling metadata upload because the metadata memory budget is exhausted"
                );
                (None, None)
            }
        },
        Err(error) => {
            warn!(
                component = "metadata",
                operation = "extract_upload_payload",
                torrent = %info_hash,
                result = "unavailable",
                error = %error,
                "disabling metadata upload because the torrent info dictionary is unavailable"
            );
            (None, None)
        }
    }
}

fn reserve_piece_assembly_bytes(
    resources: &ResourceGovernor,
    bytes: usize,
) -> anyhow::Result<MemoryLease> {
    let bytes = u64::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("piece assembly allocation does not fit in u64"))?;
    resources
        .try_acquire(MemoryClass::PieceAssembly, bytes)
        .ok_or_else(|| anyhow::anyhow!("piece assembly allocation of {bytes} bytes denied"))
}

pub(crate) fn prepare_tracker_peer_cache(
    resources: &ResourceGovernor,
    max_peers: usize,
) -> (HashSet<SocketAddr>, Option<MemoryLease>) {
    let capacity = tracker_peer_cache_cap(max_peers);
    let bytes = u64::try_from(capacity)
        .unwrap_or(u64::MAX)
        .saturating_mul(TRACKER_PEER_CACHE_BYTES_PER_ENTRY);
    let Some(memory_lease) = resources.try_acquire(MemoryClass::TrackerPeers, bytes) else {
        return (HashSet::new(), None);
    };

    let mut known = HashSet::new();
    if known.try_reserve(capacity).is_err() {
        // The governor lease is dropped with this return value, and the
        // cache remains disabled rather than allowing an infallible reserve
        // to turn allocator pressure into a process abort.
        return (HashSet::new(), None);
    }
    (known, Some(memory_lease))
}

#[cfg(test)]
fn remember_tracker_peers_bounded(
    known: &mut HashSet<SocketAddr>,
    peers: &[SocketAddr],
    cap: usize,
) -> u64 {
    let mut dropped = 0u64;
    for &peer in peers {
        if known.contains(&peer) {
            continue;
        }
        if known.len() >= cap {
            dropped = dropped.saturating_add(1);
            continue;
        }
        known.insert(peer);
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
    /// The data allocation is part of the process-wide piece-assembly
    /// budget for as long as this assembly remains live. Tests that exercise
    /// the standalone value constructor may omit the lease; production
    /// insertion always uses `with_memory_lease`.
    _memory_lease: Option<MemoryLease>,
}

impl PieceAssembly {
    #[cfg(test)]
    fn new(len: usize) -> Self {
        Self::new_with_lease(len, None)
    }

    fn with_memory_lease(len: usize, memory_lease: MemoryLease) -> Self {
        Self::new_with_lease(len, Some(memory_lease))
    }

    fn new_with_lease(len: usize, memory_lease: Option<MemoryLease>) -> Self {
        Self {
            data: vec![0; len],
            received: vec![false; len.div_ceil(MAX_BLOCK_SIZE as usize)],
            last_used: Instant::now(),
            _memory_lease: memory_lease,
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

    fn received_blocks(&self) -> Vec<(u32, bytes::Bytes)> {
        self.received
            .iter()
            .enumerate()
            .filter_map(|(block_idx, received)| {
                if !received {
                    return None;
                }
                let start = block_idx.saturating_mul(MAX_BLOCK_SIZE as usize);
                let end = start
                    .saturating_add(MAX_BLOCK_SIZE as usize)
                    .min(self.data.len());
                (start < end).then(|| {
                    (
                        start as u32,
                        bytes::Bytes::copy_from_slice(&self.data[start..end]),
                    )
                })
            })
            .collect()
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
) -> Vec<u32> {
    let mut evictions = Vec::new();
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
            evictions.push(evict_piece);
        }
    }
    evictions
}

#[derive(Debug)]
enum PeerEvent {
    Bitfield {
        peer: SocketAddr,
        id: PeerId,
        pieces: PieceBitmap,
        _memory_lease: MemoryLease,
    },
    Have {
        peer: SocketAddr,
        id: PeerId,
        piece: u32,
    },
    Unchoked {
        peer: SocketAddr,
        id: PeerId,
    },
    Choked {
        peer: SocketAddr,
        id: PeerId,
        outstanding: Vec<BlockRequest>,
    },
    Interested {
        peer: SocketAddr,
        id: PeerId,
    },
    NotInterested {
        peer: SocketAddr,
        id: PeerId,
    },
    Piece {
        peer: SocketAddr,
        id: PeerId,
        block: BlockEvent,
        _memory_lease: Option<MemoryLease>,
    },
    RequestRejected {
        peer: SocketAddr,
        id: PeerId,
        rejected: BlockRequest,
    },
    Uploaded {
        peer: SocketAddr,
        id: PeerId,
        bytes: u64,
    },
    Disconnected {
        peer: SocketAddr,
        id: PeerId,
        outstanding: Vec<BlockRequest>,
    },
    RequestTimedOut {
        peer: SocketAddr,
        id: PeerId,
        timed_out: Vec<BlockRequest>,
    },
    ExtendedHandshake {
        peer: SocketAddr,
        id: PeerId,
        ut_metadata_id: Option<u8>,
        ut_pex_id: Option<u8>,
        metadata_size: Option<u32>,
    },
    PeerExchange {
        peer: SocketAddr,
        id: PeerId,
        peers: Vec<SocketAddr>,
        dropped: Vec<SocketAddr>,
        _memory_lease: MemoryLease,
    },
}

impl PeerEvent {
    fn identity(&self) -> (SocketAddr, PeerId) {
        match self {
            Self::Bitfield { peer, id, .. }
            | Self::Have { peer, id, .. }
            | Self::Unchoked { peer, id }
            | Self::Choked { peer, id, .. }
            | Self::Interested { peer, id }
            | Self::NotInterested { peer, id }
            | Self::Piece { peer, id, .. }
            | Self::RequestRejected { peer, id, .. }
            | Self::Uploaded { peer, id, .. }
            | Self::Disconnected { peer, id, .. }
            | Self::RequestTimedOut { peer, id, .. }
            | Self::ExtendedHandshake { peer, id, .. }
            | Self::PeerExchange { peer, id, .. } => (*peer, *id),
        }
    }
}

#[derive(Debug)]
struct PeerHandle {
    id: PeerId,
    cmd_tx: mpsc::Sender<PeerCommand>,
    upload_control: RateLimitCancellation,
    shutdown_control: RateLimitCancellation,
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
    /// Dense per-peer availability maps are sized by the torrent's piece
    /// count. Keep their backing allocation under the process-wide peer
    /// memory budget for the full lifetime of the peer entry.
    _bitmap_memory_lease: MemoryLease,
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
    /// The upload-side availability bitmap is one more dense per-peer map.
    /// Keep its backing allocation charged until the peer task releases it.
    _bitmap_memory_lease: Option<MemoryLease>,
    metadata: Option<Arc<Vec<u8>>>,
    is_private: bool,
    pex_enabled: bool,
    upload_limit_bytes_per_sec: Option<u64>,
    upload_control: RateLimitCancellation,
    shutdown_control: RateLimitCancellation,
    global_download: Arc<SharedRateLimiter>,
    global_upload: Arc<SharedRateLimiter>,
}

#[derive(Clone)]
struct UploadReadContext {
    save_root: PathBuf,
    piece_map: Arc<PieceMap>,
    storage: MountScheduler,
    resources: ResourceGovernor,
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

/// Own the join handles for detached upload reads and abort them if the peer
/// loop is cancelled. Dropping a `JoinHandle` detaches its child task, so a
/// peer eviction or torrent shutdown could otherwise leave a read holding a
/// `MemoryLease` and scheduler work until its timeout.
#[derive(Default)]
struct UploadReadTasks(
    futures::stream::FuturesUnordered<tokio::task::JoinHandle<UploadReadResult>>,
);

impl std::ops::Deref for UploadReadTasks {
    type Target = futures::stream::FuturesUnordered<tokio::task::JoinHandle<UploadReadResult>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for UploadReadTasks {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for UploadReadTasks {
    fn drop(&mut self) {
        abort_upload_reads(&self.0);
    }
}

pub struct TorrentTask {
    info_hash_hex: String,
    meta: TorrentMetaV1,
    /// Exact bencoded `info` bytes used for BEP 9 upload responses. This is
    /// shared by every peer instead of being rebuilt and copied per
    /// connection. The allocation is retained under the metadata budget for
    /// the lifetime of the torrent task.
    metadata: Option<Arc<Vec<u8>>>,
    _metadata_memory_lease: Option<MemoryLease>,
    /// Incremental reservation for a durable file-policy projection whose
    /// paths are larger than the original metainfo paths. The base metadata
    /// lease covers the original runtime graph; this lease tracks replacements
    /// applied by `apply_file_policy_from_db` and is refreshed on every path
    /// change so a database rename cannot bypass the governor.
    _file_policy_memory_lease: Option<MemoryLease>,
    /// Lifetime reservation for the parsed file/tracker graph retained by the
    /// task. This is separate from the exact BEP9 payload lease above because
    /// the latter can be disabled without discarding transfer metadata.
    _torrent_metadata_memory_lease: Option<MemoryLease>,
    /// Reservation for the current tracker tier projection. This is kept as
    /// one or more leases so an update can acquire only its growth before the
    /// old projection is replaced; a failed admission therefore leaves the
    /// previous tracker set and its accounting intact.
    _tracker_state_memory_leases: Vec<MemoryLease>,
    /// Backing storage for the picker and availability counts. The engine
    /// reserves this before constructing a live task so dense piece state
    /// cannot bypass the process-wide memory governor.
    _piece_index_memory_lease: Option<MemoryLease>,
    /// The immutable paths from the metainfo. `meta.files` is the runtime
    /// projection and may carry durable client-side rename overrides; keeping
    /// the wire paths lets a policy reload recover cleanly if the file
    /// projection is absent.
    metainfo_files: Vec<TorrentFileV1>,
    save_root: PathBuf,
    piece_map: Arc<PieceMap>,
    storage: MountScheduler,
    fastresume: FastresumeStore,
    tracker_tiers: Vec<Vec<TrackerState>>,
    active_tracker_tier: usize,
    private_tracker_key: Option<TrackerKey>,
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
    /// The cache is pre-sized and its reservation covers the hash table's
    /// bounded capacity for the task lifetime. This prevents tracker, DHT,
    /// PEX, and manual peer churn from growing an ungoverned address cache.
    _tracker_peer_cache_memory_lease: Option<MemoryLease>,
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
    /// Partial pieces restored from fastresume already have their received
    /// bytes on disk, but their contents are not hydrated into an in-memory
    /// assembly. Keep those pieces on the direct-write path until they either
    /// verify or are rejected; otherwise a new block would be combined with
    /// zero-filled bytes and the completed piece would fail verification.
    restored_partial_pieces: HashSet<u32>,
    piece_assembly_bytes: usize,
    piece_assembly_soft_cap_bytes: usize,
    piece_assembly_evictions: u64,
    peer_request_window_reductions: u64,
    peer_command_queue_full: u64,
    tracker_peer_cache_drops: u64,
    dirty_pieces_since_barrier: DirtyPieceTracker,
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
    /// The lifecycle state to restore when a recheck was requested for a
    /// dormant/paused task. Dormant tasks are constructed with `paused = true`
    /// even when their durable state was active, so the runtime boolean alone
    /// cannot distinguish "paused by the user" from "temporarily quiesced
    /// until the recheck command arrives".
    recheck_restore_state: Option<TorrentState>,
    /// Durable state captured when a dormant task is promoted. Most dormant
    /// tasks stage through `Paused`, but an `Error` task must remain `Error`
    /// until an explicit resume/recheck command can move it to `Checking`.
    initial_state: TorrentState,
    max_peers: usize,
    torrent_max_peers: Option<usize>,
    pex_enabled: bool,
    event_retention: usize,
}

impl Drop for TorrentTask {
    fn drop(&mut self) {
        // The engine may have to abort this actor after a full mailbox or a
        // stuck storage/tracker operation. Dropping a Tokio AbortHandle does
        // not cancel the peer task it refers to, so explicitly abort every
        // peer here as the final lifecycle fallback. Otherwise handshakes
        // can keep sockets alive after the torrent actor has disappeared.
        for peer in self.active_peers.values() {
            if let Some(abort) = &peer.abort {
                abort.abort();
            }
        }
    }
}

impl TorrentTask {
    pub(crate) fn attach_torrent_metadata_memory_lease(&mut self, lease: MemoryLease) {
        self._torrent_metadata_memory_lease = Some(lease);
    }

    fn reserve_file_policy_memory(
        &self,
        effective_files: &[TorrentFileV1],
    ) -> Result<Option<MemoryLease>, String> {
        let baseline = persistent_file_graph_memory_bytes(&self.metainfo_files);
        let required = persistent_file_graph_memory_bytes(effective_files).saturating_sub(baseline);
        if required == 0 {
            return Ok(None);
        }
        let bytes = u64::try_from(required)
            .map_err(|_| "file-policy memory estimate does not fit in u64".to_owned())?;
        self.resources
            .try_acquire(MemoryClass::Metadata, bytes)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "file-policy allocation of {bytes} bytes denied for {} files",
                    effective_files.len()
                )
            })
    }

    fn reserve_file_policy_workspace_memory(&self) -> Result<Option<MemoryLease>, String> {
        let bytes = persistent_file_graph_memory_bytes(&self.metainfo_files);
        if bytes == 0 {
            return Ok(None);
        }
        let bytes = u64::try_from(bytes)
            .map_err(|_| "file-policy workspace memory estimate does not fit in u64".to_owned())?;
        self.resources
            .try_acquire(MemoryClass::Metadata, bytes)
            .map(Some)
            .ok_or_else(|| format!("file-policy workspace allocation of {bytes} bytes denied"))
    }

    fn refresh_tracker_state_memory(
        &mut self,
        tracker_tiers: &[Vec<TrackerState>],
        outer_capacity: usize,
    ) -> Result<(), String> {
        let required = u64::try_from(tracker_state_memory_bytes(tracker_tiers, outer_capacity))
            .map_err(|_| "tracker state memory estimate does not fit in u64".to_owned())?;
        let held = self
            ._tracker_state_memory_leases
            .iter()
            .map(MemoryLease::bytes)
            .fold(0u64, u64::saturating_add);

        if required > held {
            let extra = required - held;
            let lease = self
                .resources
                .try_acquire(MemoryClass::Metadata, extra)
                .ok_or_else(|| format!("tracker state allocation of {extra} bytes denied"))?;
            self._tracker_state_memory_leases.push(lease);
        } else if required == 0 {
            self._tracker_state_memory_leases.clear();
        } else if required < held {
            // Acquire before releasing the old reservation. If the temporary
            // double reservation does not fit, retaining the larger old lease
            // is safe and avoids an uncharged replacement window.
            if let Some(lease) = self.resources.try_acquire(MemoryClass::Metadata, required) {
                let old = std::mem::replace(&mut self._tracker_state_memory_leases, vec![lease]);
                drop(old);
            }
        }
        Ok(())
    }

    // This constructor is the current dependency-injection seam for a
    // torrent actor. It is intentionally explicit while the actor context is
    // being split into storage/network/persistence components.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        mut meta: TorrentMetaV1,
        save_root: PathBuf,
        paused: bool,
        initial_state: TorrentState,
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
        piece_index_memory_lease: Option<MemoryLease>,
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
        let metainfo_files = meta.files.clone();
        // The task only needs the exact info dictionary for BEP 9 uploads;
        // retaining the complete parsed .torrent blob here would keep up to
        // MAX_TORRENT_BYTES alive for every promoted torrent. Extract the
        // leased upload payload, then release the parser-owned raw blob.
        let raw = std::mem::take(&mut meta.raw);
        let (metadata, metadata_memory_lease) =
            prepare_metadata_payload(&resources, &info_hash_hex, &raw);
        drop(raw);
        let (known_tracker_peers, tracker_peer_cache_memory_lease) =
            prepare_tracker_peer_cache(&resources, max_peers);
        if tracker_peer_cache_memory_lease.is_none() {
            warn!(
                component = "memory",
                operation = "reserve_tracker_peer_cache",
                torrent = %info_hash_hex,
                result = "disabled",
                "tracker peer cache disabled because its memory budget or allocator reserve was unavailable"
            );
        }
        let piece_map = build_piece_map(meta.piece_length, &meta.files)
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
        let metainfo_trackers = meta.all_trackers();
        let mut tracker_tiers = tracker_tiers_from_meta(&meta);
        let tracker_restore = db
            .run("restore_tracker_state", {
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
                // `torrents.trackers` is the durable client override. Keep
                // the metainfo's BEP-12 tiers for untouched torrents, but
                // treat a changed value (including an explicit empty list)
                // as authoritative after an actor restart.
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
                    operation = "restore_tracker_state",
                    torrent = %info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to restore durable tracker state; using fresh tracker session"
                );
            }
        }
        let tracker_state_memory_leases = match reserve_tracker_state_memory(
            &resources,
            &tracker_tiers,
            tracker_tiers.capacity(),
        ) {
            Ok(leases) => leases,
            Err(error) => {
                warn!(
                    component = "memory",
                    operation = "reserve_tracker_state",
                    torrent = %info_hash_hex,
                    result = "disabled",
                    error = %error,
                    "tracker state disabled because its memory budget or allocator reserve was unavailable"
                );
                // Assign a fresh empty vector rather than clearing the
                // current one: Vec::clear retains its outer allocation.
                tracker_tiers = Vec::new();
                Vec::new()
            }
        };
        let private_tracker_key = if meta.private {
            first_tracker_key(&tracker_tiers)
        } else {
            None
        };
        let mut task = TorrentTask {
            info_hash_hex,
            meta,
            metadata,
            _metadata_memory_lease: metadata_memory_lease,
            _file_policy_memory_lease: None,
            _torrent_metadata_memory_lease: None,
            _tracker_state_memory_leases: tracker_state_memory_leases,
            _piece_index_memory_lease: piece_index_memory_lease,
            metainfo_files,
            save_root,
            piece_map,
            storage,
            fastresume: FastresumeStore::new(fastresume_dir),
            tracker_tiers,
            active_tracker_tier: 0,
            private_tracker_key,
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
            known_tracker_peers,
            _tracker_peer_cache_memory_lease: tracker_peer_cache_memory_lease,
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
            restored_partial_pieces: HashSet::new(),
            piece_assembly_bytes: 0,
            piece_assembly_soft_cap_bytes: effective_piece_assembly_soft_cap(
                piece_assembly_cap_bytes,
            ),
            piece_assembly_evictions: 0,
            peer_request_window_reductions: 0,
            peer_command_queue_full: 0,
            tracker_peer_cache_drops: 0,
            dirty_pieces_since_barrier: DirtyPieceTracker::default(),
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
            recheck_restore_state: match initial_state {
                TorrentState::Paused => Some(TorrentState::Paused),
                TorrentState::Stopped => Some(TorrentState::Stopped),
                _ => None,
            },
            initial_state,
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
        // Keep the engine command receiver outside the actor while it runs so
        // a webseed fetch can be selected against lifecycle commands without
        // borrowing two fields of `self` through overlapping futures. The
        // replacement is never observed: this task owns the real receiver
        // until it exits.
        let (_, replacement_cmd_rx) = mpsc::channel(1);
        let mut cmd_rx = std::mem::replace(&mut self.cmd_rx, replacement_cmd_rx);
        let mut pending_command = None;
        let restored = self.restore_fastresume().await;
        self.persist_tracker_state().await;
        if self.paused {
            self.persist_progress().await;
            // Dormant promotion constructs the task paused so no transfer
            // starts before the queued command is delivered. Preserve an
            // explicitly stopped projection during that staging window;
            // rewriting it as Paused makes a stopped torrent resume with the
            // wrong lifecycle state if the process exits before the command.
            let startup_state = match self.initial_state {
                TorrentState::Error => TorrentState::Error,
                // Promotion stages every dormant task paused. Preserve a
                // queued lifecycle projection during that staging window;
                // otherwise a process exit before the queued command arrives
                // silently turns queue admission into a user pause.
                TorrentState::Queued => TorrentState::Queued,
                _ => self.recheck_restore_state.unwrap_or(TorrentState::Paused),
            };
            if let Err(error) = self.set_state_checked(startup_state).await {
                warn!(
                    component = "torrent",
                    operation = "startup_state",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to persist paused startup state; stopping task"
                );
                reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
                return;
            }
        } else if self.initial_state == TorrentState::Checking || !restored {
            // `Checking` is a durable promise that the previous process had
            // not completed verification. A fastresume file may still exist
            // from before that scan, so it must never be allowed to turn the
            // task directly into Downloading/Seeding on restore. The engine
            // also recovers a queued recheck job after tasks are loaded; the
            // scan below can adopt that job id when its command arrives.
            match self
                .run_recheck_with_receiver(&mut cmd_rx, &mut pending_command, None)
                .await
            {
                RecheckOutcome::Shutdown => {
                    reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
                    return;
                }
                RecheckOutcome::Failed(error) => {
                    warn!(
                        component = "torrent",
                        operation = "startup_recheck",
                        torrent = %self.info_hash_hex,
                        result = "error",
                        error = %error,
                        "startup recheck failed; stopping task"
                    );
                    reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
                    return;
                }
                RecheckOutcome::Complete
                | RecheckOutcome::Paused { .. }
                | RecheckOutcome::Cancelled => {}
            }
        } else if self.picker.is_complete() {
            if let Err(error) = self.set_state_checked(TorrentState::Seeding).await {
                warn!(
                    component = "torrent",
                    operation = "startup_state",
                    torrent = %self.info_hash_hex,
                    result = "error",
                    error = %error,
                    "failed to persist seeding startup state; stopping task"
                );
                reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
                return;
            }
        } else if let Err(error) = self.set_state_checked(TorrentState::Downloading).await {
            warn!(
                component = "torrent",
                operation = "startup_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist downloading startup state; stopping task"
            );
            reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
            return;
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
                command = receive_torrent_command(&mut cmd_rx, &mut pending_command) => {
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
                        self.cancel_tracker_announces();
                        self.announce_stopped_with_control_deadline().await;
                        self.persist_progress().await;
                        self.save_fastresume(false).await;
                        self.shutdown_peers().await;
                        break;
                    };
                    match cmd {
                        TorrentCmd::Shutdown => {
                            self.cancel_tracker_announces();
                            self.announce_stopped_with_control_deadline().await;
                            self.persist_progress().await;
                            self.save_fastresume(false).await;
                            self.shutdown_peers().await;
                            break;
                        }
                        TorrentCmd::Pause { reply } => {
                            let was_paused = self.paused;
                            self.paused = true;
                            self.cancel_tracker_announces();
                            self.save_fastresume(false).await;
                            self.shutdown_peers().await;
                            let result = self.set_state_checked(TorrentState::Paused).await;
                            if let Err(error) = &result {
                                // A failed state write must not turn a
                                // rejected pause into a runtime-only pause.
                                // Restore the prior control state and let the
                                // caller retry once the durable store is
                                // available again.
                                self.paused = was_paused;
                                if !was_paused {
                                    self.restart_tracker_session();
                                }
                                warn!(
                                    component = "torrent",
                                    operation = "pause",
                                    torrent = %self.info_hash_hex,
                                    result = "error",
                                    error = %error,
                                    "failed to persist torrent pause"
                                );
                            } else {
                                self.recheck_restore_state = Some(TorrentState::Paused);
                                self.tracker_event = TrackerEvent::Started;
                            }
                            if let Some(reply) = reply {
                                let _ = reply.send(result);
                            }
                            if self.paused {
                                // Tracker shutdown is best-effort lifecycle
                                // housekeeping. Do it after the durable pause
                                // acknowledgement so a slow tracker cannot
                                // make the caller time out with a task that is
                                // already safely paused.
                                await_stopped_announce_or_queue_command(
                                    &mut self,
                                    &mut cmd_rx,
                                    &mut pending_command,
                                )
                                .await;
                            }
                        }
                        TorrentCmd::Resume { reply } => {
                            let result = self.prepare_resume().await;
                            if let Some(reply) = reply {
                                // Do not make API callers wait for a full
                                // disk recheck. `Checking` is the durable
                                // acknowledgement; the recheck continues in
                                // this actor immediately afterward.
                                let _ = reply.send(result.clone());
                            }
                            if result.is_ok() {
                                match self
                                    .run_recheck_with_receiver(
                                        &mut cmd_rx,
                                        &mut pending_command,
                                        None,
                                    )
                                    .await
                                {
                                    RecheckOutcome::Shutdown => break,
                                    RecheckOutcome::Failed(error) => {
                                        warn!(
                                            component = "torrent",
                                            operation = "resume_recheck",
                                            torrent = %self.info_hash_hex,
                                            result = "error",
                                            error = %error,
                                            "resume recheck failed; stopping task"
                                        );
                                        break;
                                    }
                                    RecheckOutcome::Complete
                                    | RecheckOutcome::Paused { .. }
                                    | RecheckOutcome::Cancelled => {}
                                }
                            }
                            // A recheck can invalidate pieces after the
                            // webseed timer has backed off because the picker
                            // was complete. Wake the scheduler immediately so
                            // repair does not wait for the long retry deadline.
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                        }
                        TorrentCmd::QuiesceForStorageMove { reply } => {
                            let was_paused = self.paused;
                            self.paused = true;
                            self.cancel_tracker_announces();
                            self.save_fastresume(false).await;
                            self.shutdown_peers().await;
                            if let Err(error) = self.set_state_checked(TorrentState::Paused).await {
                                self.paused = was_paused;
                                if !was_paused {
                                    self.restart_tracker_session();
                                }
                                let _ = reply.send(Err(format!(
                                    "failed to persist torrent quiesce state: {error}"
                                )));
                                continue;
                            }
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
                            let _ = reply.send(Ok(was_paused));
                            await_stopped_announce_or_queue_command(
                                &mut self,
                                &mut cmd_rx,
                                &mut pending_command,
                            )
                            .await;
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
                                // Any file-prepared bookkeeping refers to
                                // handles/allocations at the old path; the
                                // files now live at a verified-identical
                                // new path, so start clean rather than
                                // trust stale state across the move.
                                lock_prepared_files(&self.prepared_files).clear();
                            }
                            if !resume_paused {
                                match self.prepare_resume().await {
                                    Ok(()) => {
                                        // `Checking` is the durable handoff
                                        // point. Do not make storage-job
                                        // completion wait for a full disk
                                        // recheck, but do report failures that
                                        // would otherwise leave the task
                                        // paused after the job was marked
                                        // complete.
                                        let _ = reply.send(Ok(()));
                                        match self
                                            .run_recheck_with_receiver(
                                                &mut cmd_rx,
                                                &mut pending_command,
                                                None,
                                            )
                                            .await
                                        {
                                            RecheckOutcome::Shutdown => break,
                                            RecheckOutcome::Failed(error) => {
                                                warn!(
                                                    component = "torrent",
                                                    operation = "storage_move_resume_recheck",
                                                    torrent = %self.info_hash_hex,
                                                    result = "error",
                                                    error = %error,
                                                    "post-storage-move recheck failed; stopping task"
                                                );
                                                break;
                                            }
                                            RecheckOutcome::Complete
                                            | RecheckOutcome::Paused { .. }
                                            | RecheckOutcome::Cancelled => {}
                                        }
                                    }
                                    Err(error) => {
                                        let _ = reply.send(Err(error.clone()));
                                        warn!(
                                            component = "torrent",
                                            operation = "storage_move_resume",
                                            torrent = %self.info_hash_hex,
                                            result = "error",
                                            error = %error,
                                            "post-storage-move resume could not be persisted"
                                        );
                                    }
                                }
                                reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                            } else {
                                let _ = reply.send(Ok(()));
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
                                self.connect_priority_peers(addrs).await;
                            }
                        }
                        TorrentCmd::BanPeer(peer) => {
                            self.evict_peer(peer);
                        }
                        TorrentCmd::EvictBannedPeers => {
                            self.evict_banned_peers().await;
                        }
                        TorrentCmd::GetPeers { reply } => {
                            let _ = reply.send(Vec::new());
                        }
                        TorrentCmd::GetPeerSnapshotCount { reply } => {
                            let _ = reply.send(self.active_peers.len());
                        }
                        TorrentCmd::GetPeerSnapshots {
                            max_entries,
                            reply,
                        } => {
                            let _ = reply.send(self.peer_snapshots_limited(max_entries));
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
                                self.accept_utp_peer(*stream, peer_addr, handshake, peer_permit)
                                    .await;
                            }
                        }
                        TorrentCmd::Recheck { job_id } => {
                            match self
                                .run_recheck_with_receiver(
                                    &mut cmd_rx,
                                    &mut pending_command,
                                    job_id,
                                )
                                .await
                            {
                                RecheckOutcome::Shutdown => break,
                                RecheckOutcome::Failed(error) => {
                                    warn!(
                                        component = "torrent",
                                        operation = "recheck",
                                        torrent = %self.info_hash_hex,
                                        result = "error",
                                        error = %error,
                                        "recheck failed; stopping task"
                                    );
                                    break;
                                }
                                RecheckOutcome::Complete
                                | RecheckOutcome::Paused { .. }
                                | RecheckOutcome::Cancelled => {}
                            }
                            // Recheck may transition a complete torrent back
                            // to downloading when corruption is found.
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
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
                        TorrentCmd::UpdateTrackers { trackers, reply } => {
                            let result = self.apply_tracker_urls(trackers).await;
                            if let Err(error) = &result {
                                warn!(
                                    component = "torrent",
                                    operation = "update_trackers",
                                    torrent = %self.info_hash_hex,
                                    result = "error",
                                    error = %error,
                                    "failed to apply updated tracker URLs"
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
                    if let Some(event) = peer_event_if_active(self.paused, event) {
                        self.handle_peer_event(event).await;
                    }
                }

                Some(event) = self.peer_disconnect_rx.recv() => {
                    if let Some(event) = peer_event_if_active(self.paused, event) {
                        self.handle_peer_event(event).await;
                        if self.active_peers.is_empty() {
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                        }
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
                    // Webseed response bodies are already bounded, but the
                    // shared download bucket may be configured far below a
                    // protocol block size. Waiting for that bucket in the
                    // actor used to make Pause/Shutdown wait for hours. Keep
                    // the rate wait cancellable by the command receiver, but
                    // let disk handling finish once the budget was acquired.
                    let operation = self.download_next_webseed_block();
                    let outcome = {
                        tokio::pin!(operation);
                        tokio::select! {
                            result = &mut operation => Ok::<_, Option<TorrentCmd>>(result),
                            command = cmd_rx.recv() => Err(command),
                        }
                    };
                    match outcome {
                        Ok(Some(block)) => {
                            self.handle_block(block).await;
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                        }
                        Ok(None) => {
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                        }
                        Err(Some(command)) => {
                            // The canceled operation may have reserved a
                            // picker request before it reached its cleanup
                            // path. There are no active peers under this
                            // branch, so reset only the webseed-side request
                            // state before replaying the command through the
                            // normal actor path.
                            self.picker.reset_outstanding_requests();
                            pending_command = Some(command);
                            reset_webseed_sleep(&mut webseed_sleep, self.webseed_wake_delay());
                        }
                        Err(None) => {
                            warn!(
                                component = "torrent",
                                operation = "run",
                                torrent = %self.info_hash_hex,
                                result = "command_channel_closed",
                                "torrent command channel closed; shutting down"
                            );
                            self.cancel_tracker_announces();
                            self.announce_stopped_with_control_deadline().await;
                            self.persist_progress().await;
                            self.save_fastresume(false).await;
                            self.shutdown_peers().await;
                            break;
                        }
                    }
                }
            }
        }

        // A Shutdown command wins FIFO ordering, but lifecycle/query commands
        // may already be queued behind it while the bounded cleanup above is
        // running. Resolve their replies before dropping the receiver so
        // callers observe a terminal error/result instead of a silent oneshot
        // disconnect.
        reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);
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

    async fn start_due_tracker_announces(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }

        let tier_idx = self.active_tracker_tier.min(self.tracker_tiers.len() - 1);
        let available = self.tracker_workers.available();
        if available == 0 {
            return;
        }
        let candidates = tracker_keys_for_announce(
            &self.tracker_tiers,
            tier_idx,
            self.meta.private,
            self.private_tracker_key,
        )
        .into_iter()
        .filter_map(|key @ (selected_tier, tracker_index)| {
            let tracker = self.tracker_tiers.get(selected_tier)?.get(tracker_index)?;
            (tracker.is_due() && !self.tracker_workers.contains(key)).then(|| TrackerAnnounceSpec {
                key,
                url: tracker.url.clone(),
                tracker_id: tracker.tracker_id.clone(),
                event: self.tracker_event,
            })
        })
        .take(available)
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
        let failed = result.response.is_err();
        match result.response {
            Ok(resp) => {
                let peers: Vec<SocketAddr> = resp.peers.iter().map(|peer| peer.addr).collect();
                tracker.on_success_with_min_interval(&resp, self.min_announce_interval);
                if let Some(scrape) = result.scrape {
                    tracker.scrape_complete = Some(scrape.complete);
                    tracker.scrape_incomplete = Some(scrape.incomplete);
                    tracker.scrape_downloaded = Some(scrape.downloaded);
                }
                self.persist_tracker_state().await;
                self.tracker_event = tracker_event_after_success(self.tracker_event, result.event);
                if !peers.is_empty() && !self.paused {
                    self.remember_tracker_peers(&peers);
                    info!(
                        torrent = %self.info_hash_hex,
                        tracker = %url_log_target(&result.url),
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
                    tracker = %url_log_target(&result.url),
                    result = "error",
                    error = %err,
                    "tracker announce failed"
                );
                tracker.on_failure(err);
                self.persist_tracker_state().await;
            }
        }
        if self.meta.private {
            if failed {
                self.advance_private_tracker_after_failure((tier_idx, tracker_idx));
            }
        } else {
            self.maybe_advance_tracker_tier(tier_idx);
        }
    }

    fn tracker_announce_context(&self, uploaded: u64, downloaded: u64) -> TrackerAnnounceContext {
        TrackerAnnounceContext {
            info_hash: self.meta.info_hash,
            uploaded,
            downloaded,
            left: self.picker.bytes_left(),
            listen_port: match self.network_budget.listen_port() {
                0 => self.listen_port,
                port => port,
            },
            http_timeout: self.http_timeout,
            udp_timeout: self.udp_timeout,
            numwant: protocol_numwant(self.peer_capacity()),
            egress_policy: self.egress_policy,
            resources: self.resources.clone(),
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

    fn advance_private_tracker_after_failure(&mut self, failed: TrackerKey) {
        if self.private_tracker_key != Some(failed) {
            return;
        }
        let Some(next) = next_tracker_key(&self.tracker_tiers, failed) else {
            return;
        };
        if next == failed {
            return;
        }

        self.cancel_tracker_announces();
        self.disconnect_private_tracker_peers();
        self.private_tracker_key = Some(next);
        self.active_tracker_tier = next.0;
        if let Some(tracker) = self
            .tracker_tiers
            .get_mut(next.0)
            .and_then(|tier| tier.get_mut(next.1))
        {
            tracker.schedule_immediate();
        }
        debug!(
            component = "tracker",
            operation = "private_failover",
            torrent = %self.info_hash_hex,
            from_tier = failed.0,
            to_tier = next.0,
            result = "switched",
            "private torrent advanced to the next tracker after failure"
        );
    }

    fn disconnect_private_tracker_peers(&mut self) {
        self.known_tracker_peers.clear();
        let peers = self.active_peers.keys().copied().collect::<Vec<_>>();
        for peer in peers {
            self.evict_peer(peer);
        }
    }

    async fn announce_stopped(&mut self) {
        if !consume_stopped_announce(&mut self.stopped_announced) {
            return;
        }

        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let context = self.tracker_announce_context(uploaded, downloaded);
        let candidates = tracker_keys_for_stopped_announce(
            &self.tracker_tiers,
            self.meta.private,
            self.private_tracker_key,
        )
        .into_iter()
        .filter_map(|(tier_idx, tracker_idx)| {
            let tracker = self.tracker_tiers.get(tier_idx)?.get(tracker_idx)?;
            Some((
                tier_idx,
                tracker_idx,
                tracker.url.clone(),
                tracker.tracker_id.clone(),
            ))
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
                    .await
                    .map(|response| response.response);
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
                Ok(resp) => tracker.on_success_with_min_interval(&resp, self.min_announce_interval),
                Err(err) => {
                    warn!(
                        component = "tracker",
                        operation = "announce_stopped",
                        torrent = %self.info_hash_hex,
                        tracker = %url_log_target(&url),
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
                "stopped tracker announce exceeded the lifecycle control deadline"
            );
        }
    }

    fn restart_tracker_session(&mut self) {
        self.cancel_tracker_announces();
        self.tracker_event = TrackerEvent::Started;
        self.stopped_announced = false;
        // A stopped announce records the tracker's ordinary next interval.
        // Resuming is a new tracker session, so that old deadline must not
        // delay the Started event until the previous session's interval has
        // elapsed.
        self.schedule_trackers_now();
    }

    fn cancel_tracker_announces(&mut self) {
        self.tracker_workers.cancel();
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
        let mut tracker_count = 0u64;
        let mut tracker_snapshot_bytes = 0u64;
        // TorrentNG-client counterpart to the compatible-client service's
        // cached `t.message`
        // column: a torrent can be actively seeding/downloading fine while
        // its tracker rejects announces, which `state` alone never
        // reflects. Cache the first error/warning message found across
        // any tier on the registry entry so list/facet queries can read it
        // directly instead of needing a per-torrent round trip through
        // this actor.
        let mut tracker_message: Option<String> = None;
        for (tier_idx, tier) in self.tracker_tiers.iter().enumerate() {
            for tracker in tier {
                let failure_reason = tracker_failure_reason(&tracker.status);
                let warning_message = tracker_warning_message(&tracker.status);
                let row_bytes = 256u64
                    .saturating_add(self.info_hash_hex.len() as u64)
                    .saturating_add(tracker.url.len() as u64)
                    .saturating_add(tracker_status_label(&tracker.status).len() as u64)
                    .saturating_add(
                        tracker
                            .tracker_id
                            .as_ref()
                            .map_or(0, |tracker_id| tracker_id.len() as u64),
                    )
                    .saturating_add(
                        failure_reason
                            .as_ref()
                            .map_or(0, |message| message.len() as u64),
                    )
                    .saturating_add(
                        warning_message
                            .as_ref()
                            .map_or(0, |message| message.len() as u64),
                    );
                tracker_count = tracker_count.saturating_add(1);
                tracker_snapshot_bytes = tracker_snapshot_bytes.saturating_add(row_bytes);
                crate::engine::validate_tracker_snapshot_size(
                    tracker_count,
                    tracker_snapshot_bytes,
                )?;

                if tracker_message.is_none() {
                    tracker_message = failure_reason.clone().or_else(|| warning_message.clone());
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
                    failure_reason,
                    warning_message,
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
        if self.meta.private {
            if let Some(key) = self
                .private_tracker_key
                .or_else(|| first_tracker_key(&self.tracker_tiers))
            {
                if let Some(tracker) = self
                    .tracker_tiers
                    .get_mut(key.0)
                    .and_then(|tier| tier.get_mut(key.1))
                {
                    tracker.schedule_immediate();
                }
            }
        } else {
            for tier in &mut self.tracker_tiers {
                for tracker in tier {
                    tracker.schedule_immediate();
                }
            }
        }
    }

    async fn apply_tracker_urls(&mut self, trackers: Vec<String>) -> Result<(), String> {
        let tracker_tiers = tracker_tiers_from_urls(&trackers);
        self.refresh_tracker_state_memory(&tracker_tiers, tracker_tiers.capacity())?;
        self.cancel_tracker_announces();
        if self.meta.private {
            self.disconnect_private_tracker_peers();
        }
        self.tracker_tiers = tracker_tiers;
        self.private_tracker_key = if self.meta.private {
            first_tracker_key(&self.tracker_tiers)
        } else {
            None
        };
        self.active_tracker_tier = self.private_tracker_key.map_or(0, |(tier, _)| tier);
        self.tracker_event = TrackerEvent::Empty;
        self.schedule_trackers_now();

        {
            let mut registry = self.registry.write().await;
            if let Some(mut entry) = registry.get_mut(&self.info_hash_hex) {
                entry.tracker_message = None;
            };
        }
        // The engine persists the compact URL override and fresh detail rows
        // before sending this command. A tracker result that was already
        // waiting on the DB worker can nevertheless complete after that
        // transaction and overwrite the detail rows with its older snapshot.
        // Re-persist the projection after installing the new URLs so the
        // command acknowledgement also fences that stale detail write.
        self.persist_tracker_state().await;
        Ok(())
    }

    fn schedule_active_tracker_tier_now(&mut self) {
        if self.tracker_tiers.is_empty() {
            return;
        }
        let tier_idx = self.active_tracker_tier.min(self.tracker_tiers.len() - 1);
        for (selected_tier, tracker_idx) in tracker_keys_for_announce(
            &self.tracker_tiers,
            tier_idx,
            self.meta.private,
            self.private_tracker_key,
        ) {
            if let Some(tracker) = self
                .tracker_tiers
                .get_mut(selected_tier)
                .and_then(|tier| tier.get_mut(tracker_idx))
            {
                tracker.schedule_immediate();
            }
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
        let has_file_policy = !rows.is_empty();
        // Admission must precede the clone and replacement map construction;
        // reserving only after those allocations lets a large durable policy
        // briefly bypass the metadata governor during reload.
        let _file_policy_workspace_memory_lease = self.reserve_file_policy_workspace_memory()?;
        let mut effective_files = self.metainfo_files.clone();
        let mut policy = HashMap::with_capacity(rows.len());
        for row in rows {
            let file_index = u32::try_from(row.file_index).map_err(|_| {
                format!(
                    "persisted file policy has invalid file index {}",
                    row.file_index
                )
            })?;
            let Some(file) = effective_files
                .iter_mut()
                .find(|file| file.index == file_index)
            else {
                return Err(format!(
                    "persisted file policy references unknown file index {file_index}"
                ));
            };
            file.path = parse_persisted_file_path(&row.path)?;
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

        crate::engine::validate_file_path_projection(
            effective_files
                .iter()
                .filter(|file| !file.pad)
                .map(|file| file.path.as_display()),
        )?;

        let paths_changed = self.meta.files.len() != effective_files.len()
            || self
                .meta
                .files
                .iter()
                .zip(&effective_files)
                .any(|(current, effective)| {
                    current.index != effective.index || current.path != effective.path
                });
        if paths_changed {
            let mut changed_file_indices = self
                .meta
                .files
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
                .map_err(|error| format!("invalidating renamed file pieces: {error}"))?;
            let file_policy_memory_lease = self.reserve_file_policy_memory(&effective_files)?;
            // Peer upload contexts hold an Arc<PieceMap> and a copy of the
            // storage root. End those sessions before publishing the new map,
            // otherwise an in-flight peer can keep reading the old path after
            // the rename has been accepted.
            let piece_map = build_piece_map(self.meta.piece_length, &effective_files)?;
            self.shutdown_peers().await;
            // A path projection change changes the bytes represented by every
            // overlapping piece: the old file may still exist, the new file
            // may be missing, or an operator may have moved a replacement
            // into place. Do not retain the old picker result across that
            // boundary, or the task can remain falsely complete/seeding and
            // serve data from an unverified path.
            for (first, last) in changed_piece_ranges {
                for piece in first..last {
                    self.picker.reject_piece(piece as usize);
                    self.restored_partial_pieces.remove(&piece);
                }
            }
            self.meta.files = effective_files;
            self.piece_map = piece_map;
            self._file_policy_memory_lease = file_policy_memory_lease;
            lock_prepared_files(&self.prepared_files).clear();
        }

        let piece_count = self.piece_map.piece_count as usize;
        if !has_file_policy {
            // No per-file projection means the metainfo defaults apply. This
            // also clears a stale policy after the durable projection is
            // intentionally removed.
            self.apply_piece_policy(&vec![true; piece_count], vec![false; piece_count]);
            return Ok(());
        }
        let file_lengths: HashMap<u32, u64> = self
            .meta
            .files
            .iter()
            .map(|file| (file.index, file.length))
            .collect();
        let mut enabled = vec![false; piece_count];
        let mut priority = vec![false; piece_count];
        for piece in 0..self.piece_map.piece_count {
            let regions = self
                .piece_map
                .piece_to_file_regions(piece)
                .map_err(|error| format!("building file policy for piece {piece}: {error}"))?;
            let mut any_wanted = false;
            let mut any_high = false;
            let mut any_first_last = false;
            for region in regions {
                if region.pad {
                    continue;
                }
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
                priority[piece as usize] = true;
            }
        }
        self.apply_piece_policy(&enabled, priority);
        Ok(())
    }

    fn apply_piece_policy(&mut self, enabled: &[bool], priority: Vec<bool>) {
        for (piece, enabled) in enabled.iter().copied().enumerate() {
            self.picker.set_piece_enabled(piece, enabled);
        }
        self.picker.set_priority_mask(priority);
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
        self.recheck_restore_state = Some(TorrentState::Paused);
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
        self.download_tokens = self
            .download_limit_bytes_per_sec
            .map(download_bucket_capacity)
            .unwrap_or(u64::MAX);
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
                Ok(()) => {
                    handle.pending_upload_limit = None;
                    handle.upload_control.cancel();
                }
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

    async fn prepare_resume(&mut self) -> Result<(), String> {
        let was_paused = self.paused;
        self.paused = false;
        self.restart_tracker_session();
        if let Err(error) = self.set_state_checked(TorrentState::Checking).await {
            self.paused = was_paused;
            if was_paused {
                self.cancel_tracker_announces();
            }
            return Err(format!("failed to persist torrent resume state: {error}"));
        }
        self.recheck_restore_state = None;
        Ok(())
    }

    #[cfg(test)]
    async fn run_recheck(&mut self, job_id: Option<String>) -> RecheckOutcome {
        let (_, replacement_cmd_rx) = mpsc::channel(1);
        let mut cmd_rx = std::mem::replace(&mut self.cmd_rx, replacement_cmd_rx);
        let mut pending_command = None;
        let outcome = self
            .run_recheck_with_receiver(&mut cmd_rx, &mut pending_command, job_id)
            .await;
        self.cmd_rx = cmd_rx;
        outcome
    }

    async fn run_recheck_with_receiver(
        &mut self,
        cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
        pending_command: &mut Option<TorrentCmd>,
        mut job_id: Option<String>,
    ) -> RecheckOutcome {
        let mut valid = 0usize;
        let mut verified_bytes = 0_u64;
        // The picker is updated as each piece is verified, so retaining a
        // second result entry for every piece only multiplied recheck memory
        // for large torrents. Keep the job-facing invalid sample bounded at
        // the same limit enforced by the database, while the accumulator
        // retains the complete count for progress events.
        let mut invalid_pieces = RecheckInvalidPieces::new(self.piece_map.piece_count);
        // A recheck supersedes any announce already in flight. Without a
        // generation bump, a stale Started/Completed response can be handled
        // after a pause or cancellation and mutate the tracker session that
        // the recheck is supposed to have frozen.
        self.cancel_tracker_announces();
        // `shutdown_peers` also clears in-memory piece assemblies. Flush
        // those received blocks first so starting a recheck cannot discard
        // progress that has not yet crossed the normal complete-piece write
        // boundary.
        self.save_fastresume(false).await;
        self.shutdown_peers().await;
        // Do not checkpoint the old fastresume bitmap while this scan is in
        // progress. Publish each piece below only after its current bytes
        // have been hashed, so a crash cannot restore stale-valid pieces.
        for piece in 0..self.piece_map.piece_count {
            self.picker.reject_piece(piece as usize);
        }
        if let Err(error) = self.set_state_checked(TorrentState::Checking).await {
            self.paused = true;
            self.cancel_tracker_announces();
            await_stopped_announce_or_queue_command(self, cmd_rx, pending_command).await;
            if let Some(job_id) = &job_id {
                self.persist_recheck_job_progress(
                    job_id,
                    0,
                    verified_bytes,
                    &invalid_pieces,
                    JOB_STATE_FAILED,
                    Some("failed to persist torrent checking state"),
                )
                .await;
            }
            return RecheckOutcome::Failed(format!(
                "failed to persist torrent checking state: {error}"
            ));
        }

        for piece in 0..self.piece_map.piece_count {
            match self
                .pending_recheck_control(cmd_rx, pending_command, &mut job_id)
                .await
            {
                Some(RecheckOutcome::Paused {
                    reply,
                    previous_paused,
                    previous_restore_state,
                    state_persisted,
                }) => {
                    self.save_fastresume(false).await;
                    let result = if state_persisted {
                        Ok(())
                    } else {
                        self.set_state_checked(TorrentState::Paused)
                            .await
                            .map_err(|error| {
                                format!("failed to persist torrent pause after recheck: {error}")
                            })
                    };
                    if let Err(error) = &result {
                        warn!(
                            component = "torrent",
                            operation = "pause_during_recheck",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to persist torrent pause after recheck"
                        );
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(result.clone());
                    }
                    if result.is_err() {
                        // The pause request was rejected by durable storage.
                        // Restore the pre-request runtime intent and keep the
                        // recheck running against its still-Checking state;
                        // otherwise the task would remain paused while the
                        // registry/database still claim verification is active.
                        self.paused = previous_paused;
                        self.recheck_restore_state = previous_restore_state;
                        continue;
                    }
                    if let Some(job_id) = &job_id {
                        self.persist_recheck_job_progress(
                            job_id,
                            piece,
                            verified_bytes,
                            &invalid_pieces,
                            if result.is_ok() {
                                JOB_STATE_PAUSED
                            } else {
                                JOB_STATE_FAILED
                            },
                            if result.is_ok() {
                                Some("recheck paused")
                            } else {
                                Some("failed to persist torrent pause after recheck")
                            },
                        )
                        .await;
                    }
                    await_stopped_announce_or_queue_command(self, cmd_rx, pending_command).await;
                    return RecheckOutcome::Paused {
                        reply: None,
                        previous_paused,
                        previous_restore_state,
                        state_persisted: true,
                    };
                }
                Some(RecheckOutcome::Cancelled) => {
                    let restore_state = self.recheck_restore_state.unwrap_or(TorrentState::Paused);
                    self.paused =
                        matches!(restore_state, TorrentState::Paused | TorrentState::Stopped);
                    self.recheck_restore_state = Some(restore_state);
                    self.cancel_tracker_announces();
                    self.save_fastresume(false).await;
                    await_stopped_announce_or_queue_command(self, cmd_rx, pending_command).await;
                    let lifecycle_persisted = if let Err(error) =
                        self.set_state_checked(restore_state).await
                    {
                        warn!(
                            component = "torrent",
                            operation = "cancel_recheck",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to persist torrent lifecycle state after recheck cancellation"
                        );
                        false
                    } else {
                        true
                    };
                    if lifecycle_persisted {
                        if let Some(job_id) = &job_id {
                            self.persist_recheck_job_progress(
                                job_id,
                                piece,
                                verified_bytes,
                                &invalid_pieces,
                                JOB_STATE_CANCELLED,
                                Some("recheck cancelled"),
                            )
                            .await;
                        }
                    } else {
                        // Keep the durable job in `cancelling` when the
                        // restored torrent state was not committed. A
                        // terminal job would be ignored by restart recovery
                        // while the torrent row still says `Checking`.
                        warn!(
                            component = "torrent",
                            operation = "cancel_recheck",
                            torrent = %self.info_hash_hex,
                            result = "recoverable",
                            "recheck cancellation remains recoverable until torrent state persistence succeeds"
                        );
                    }
                    return RecheckOutcome::Cancelled;
                }
                Some(RecheckOutcome::Shutdown) => {
                    // A disconnected command channel reaches this branch
                    // without the explicit Shutdown arm below. Keep the
                    // terminal tracker event best effort for both cases.
                    self.announce_stopped_with_control_deadline().await;
                    self.save_fastresume(false).await;
                    if let Err(error) = self.set_state_checked(TorrentState::Stopped).await {
                        warn!(
                            component = "torrent",
                            operation = "shutdown_during_recheck",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to persist stopped state after recheck shutdown"
                        );
                    }
                    if let Some(job_id) = &job_id {
                        self.persist_recheck_job_progress(
                            job_id,
                            piece,
                            verified_bytes,
                            &invalid_pieces,
                            JOB_STATE_PAUSED,
                            Some("recheck interrupted by shutdown"),
                        )
                        .await;
                    }
                    return RecheckOutcome::Shutdown;
                }
                Some(RecheckOutcome::Complete) | None => {}
                Some(RecheckOutcome::Failed(error)) => {
                    return RecheckOutcome::Failed(error);
                }
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
                    valid += 1;
                    verified_bytes = verified_bytes.saturating_add(
                        self.piece_length(piece).map(u64::from).unwrap_or_default(),
                    );
                    self.picker.mark_have(piece as usize);
                }
                VerifyResult::Invalid => {
                    invalid_pieces.record(piece);
                    self.picker.reject_piece(piece as usize);
                }
                VerifyResult::Missing { .. } => {
                    invalid_pieces.record(piece);
                    self.picker.reject_piece(piece as usize);
                }
            }

            if piece > 0 && piece % 64 == 0 {
                self.save_fastresume(false).await;
                if let Some(job_id) = &job_id {
                    self.persist_recheck_job_progress(
                        job_id,
                        piece + 1,
                        verified_bytes,
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
            invalid = invalid_pieces.total,
            "recheck complete"
        );
        self.save_fastresume(true).await;

        let final_state = if self.paused {
            if let Some(restore_state) = self.recheck_restore_state {
                // A recheck requested for a paused or stopped torrent must
                // not silently start it. Dormant tasks are also constructed
                // paused, so this branch is gated by the explicit restore
                // intent rather than by `self.paused` alone.
                self.tracker_event = TrackerEvent::Started;
                self.cancel_tracker_announces();
                restore_state
            } else {
                // Promotion temporarily starts an active dormant torrent in
                // the paused runtime mode while its recheck command is being
                // delivered. Once verification completes, restore activity.
                self.paused = false;
                self.restart_tracker_session();
                if self.picker.is_complete() {
                    self.tracker_event = TrackerEvent::Completed;
                    self.schedule_trackers_now();
                    TorrentState::Seeding
                } else {
                    self.persist_progress().await;
                    TorrentState::Downloading
                }
            }
        } else if self.picker.is_complete() {
            self.tracker_event = TrackerEvent::Completed;
            self.schedule_trackers_now();
            TorrentState::Seeding
        } else {
            self.tracker_event = tracker_event_after_incomplete_recheck(self.tracker_event);
            // Rechecking cancelled any announce from the previous state. The
            // tracker needs a fresh left/count snapshot when the torrent is
            // found incomplete, even if the old session deadline was still in
            // the future.
            self.schedule_trackers_now();
            self.persist_progress().await;
            TorrentState::Downloading
        };
        if let Err(error) = self.set_state_checked(final_state).await {
            self.paused = true;
            self.tracker_event = TrackerEvent::Started;
            self.cancel_tracker_announces();
            await_stopped_announce_or_queue_command(self, cmd_rx, pending_command).await;
            self.shutdown_peers().await;
            if let Some(job_id) = &job_id {
                self.persist_recheck_job_progress(
                    job_id,
                    self.piece_map.piece_count,
                    verified_bytes,
                    &invalid_pieces,
                    JOB_STATE_FAILED,
                    Some("failed to persist torrent state after recheck"),
                )
                .await;
            }
            return RecheckOutcome::Failed(format!(
                "failed to persist torrent state after recheck: {error}"
            ));
        }
        if let Some(job_id) = &job_id {
            self.persist_recheck_job_progress(
                job_id,
                self.piece_map.piece_count,
                verified_bytes,
                &invalid_pieces,
                JOB_STATE_COMPLETED,
                Some("recheck completed"),
            )
            .await;
        }
        RecheckOutcome::Complete
    }

    async fn pending_recheck_control(
        &mut self,
        cmd_rx: &mut mpsc::Receiver<TorrentCmd>,
        pending_command: &mut Option<TorrentCmd>,
        active_job_id: &mut Option<String>,
    ) -> Option<RecheckOutcome> {
        loop {
            match cmd_rx.try_recv() {
                Ok(TorrentCmd::Pause { reply }) => {
                    let previous_paused = self.paused;
                    let previous_restore_state = self.recheck_restore_state;
                    self.paused = true;
                    self.recheck_restore_state = Some(TorrentState::Paused);
                    self.cancel_tracker_announces();
                    self.shutdown_peers().await;
                    return Some(RecheckOutcome::Paused {
                        reply,
                        previous_paused,
                        previous_restore_state,
                        state_persisted: false,
                    });
                }
                Ok(TorrentCmd::Shutdown) => {
                    self.paused = true;
                    self.cancel_tracker_announces();
                    self.shutdown_peers().await;
                    self.announce_stopped_with_control_deadline().await;
                    return Some(RecheckOutcome::Shutdown);
                }
                Ok(TorrentCmd::Resume { reply }) => {
                    self.paused = false;
                    self.recheck_restore_state = None;
                    // A paused torrent normally has no tracker workers left;
                    // rechecking it keeps the actor inside this control path,
                    // so the regular `prepare_resume` helper is not reached
                    // when the job is resumed here.
                    self.restart_tracker_session();
                    if let Some(reply) = reply {
                        // Recheck already owns the durable `Checking` state;
                        // acknowledge the control request without waiting for
                        // the current verification pass to finish.
                        let _ = reply.send(Ok(()));
                    }
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
                Ok(TorrentCmd::UpdateTrackers { trackers, reply }) => {
                    let result = self.apply_tracker_urls(trackers).await;
                    if let Err(error) = &result {
                        warn!(
                            component = "torrent",
                            operation = "update_trackers",
                            torrent = %self.info_hash_hex,
                            result = "error",
                            error = %error,
                            "failed to apply updated tracker URLs during recheck"
                        );
                    }
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
                Ok(TorrentCmd::UpdatePeerExchange(enabled)) => {
                    self.pex_enabled = enabled;
                }
                Ok(TorrentCmd::Recheck { job_id }) => {
                    if let Some(job_id) = job_id {
                        if active_job_id.is_none() {
                            // Startup/resume rechecks normally have no durable
                            // job. If an explicit recheck is queued while one
                            // of those scans is still running, adopt its job
                            // identity so the scan completes the job instead
                            // of silently discarding the command.
                            *active_job_id = Some(job_id);
                        } else if active_job_id.as_deref() != Some(job_id.as_str()) {
                            debug!(
                                torrent = %self.info_hash_hex,
                                active_job_id = ?active_job_id,
                                queued_job_id = %job_id,
                                "ignoring recheck command for a different active job"
                            );
                        }
                    }
                }
                Ok(TorrentCmd::CancelJob { job_id }) => {
                    if active_job_id.as_deref() != Some(job_id.as_str()) {
                        debug!(
                            torrent = %self.info_hash_hex,
                            job_id,
                            active_job_id = ?active_job_id,
                            "ignoring cancellation for an inactive recheck job"
                        );
                        continue;
                    }
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
                Ok(TorrentCmd::EvictBannedPeers) => {
                    self.evict_banned_peers().await;
                }
                Ok(TorrentCmd::GetPeers { reply }) => {
                    let _ = reply.send(Vec::new());
                }
                Ok(TorrentCmd::GetPeerSnapshotCount { reply }) => {
                    let _ = reply.send(0);
                }
                Ok(TorrentCmd::GetPeerSnapshots { reply, .. }) => {
                    let _ = reply.send(Ok(Vec::new()));
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
                    self.cancel_tracker_announces();
                    self.shutdown_peers().await;
                    if let Err(error) = self.set_state_checked(TorrentState::Paused).await {
                        self.paused = was_paused;
                        if !was_paused {
                            self.restart_tracker_session();
                        }
                        let _ = reply.send(Err(format!(
                            "failed to persist torrent quiesce state: {error}"
                        )));
                        continue;
                    }
                    let _ = reply.send(Ok(was_paused));
                    await_stopped_announce_or_queue_command(self, cmd_rx, pending_command).await;
                    return Some(RecheckOutcome::Paused {
                        reply: None,
                        previous_paused: was_paused,
                        previous_restore_state: self.recheck_restore_state,
                        state_persisted: true,
                    });
                }
                Ok(TorrentCmd::ResumeAfterStorageMove {
                    new_save_root,
                    resume_paused,
                    reply,
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
                        lock_prepared_files(&self.prepared_files).clear();
                    }
                    self.paused = resume_paused;
                    self.recheck_restore_state = resume_paused.then_some(TorrentState::Paused);
                    if !resume_paused {
                        self.restart_tracker_session();
                    }
                    let _ = reply.send(Ok(()));
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
            if self
                .picker
                .partial_pieces()
                .iter()
                .any(|(piece, _)| *piece == partial.piece)
            {
                self.restored_partial_pieces.insert(partial.piece);
            }
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
        let block_len = u32::try_from(block.data.len())
            .map_err(|_| anyhow::anyhow!("peer block length does not fit in u32"))?;
        let regions = self
            .piece_map
            .validate_request(block.piece, block.offset, block_len)?;
        let mut data_offset = 0usize;

        for region in regions {
            let region_len = usize::try_from(region.length)
                .map_err(|_| anyhow::anyhow!("piece region length does not fit in memory"))?;
            let end = data_offset
                .checked_add(region_len)
                .ok_or_else(|| anyhow::anyhow!("piece region data offset overflow"))?;
            let region_data = block
                .data
                .get(data_offset..end)
                .ok_or_else(|| anyhow::anyhow!("peer block does not cover mapped piece regions"))?;
            if region.pad {
                if region_data.iter().any(|byte| *byte != 0) {
                    anyhow::bail!("peer block contains non-zero bytes for BEP 47 padding");
                }
                data_offset = end;
                continue;
            }
            let file = self
                .meta
                .files
                .iter()
                .find(|file| file.index == region.file_index)
                .ok_or_else(|| anyhow::anyhow!("file index {} out of range", region.file_index))?;
            let path = file.path.resolve(&self.save_root);
            self.prepare_file_once(file.index, &path, file.length)
                .await?;
            let data = bytes::Bytes::copy_from_slice(region_data);
            data_offset = end;
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

        if data_offset != block.data.len() {
            anyhow::bail!("mapped piece regions do not cover the peer block");
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
            if region.pad {
                continue;
            }
            let file = self
                .meta
                .files
                .iter()
                .find(|file| file.index == region.file_index)
                .ok_or_else(|| anyhow::anyhow!("file index {} out of range", region.file_index))?;
            let path = file.path.resolve(&self.save_root);
            self.prepare_file_once(file.index, &path, file.length)
                .await?;
            let start = usize::try_from(region.piece_offset)
                .map_err(|_| anyhow::anyhow!("piece region offset does not fit in memory"))?;
            let region_len = usize::try_from(region.length)
                .map_err(|_| anyhow::anyhow!("piece region length does not fit in memory"))?;
            let end = start
                .checked_add(region_len)
                .ok_or_else(|| anyhow::anyhow!("piece region data offset overflow"))?;
            let region_data = assembly
                .data
                .get(start..end)
                .ok_or_else(|| anyhow::anyhow!("piece assembly does not cover mapped region"))?;
            let data = bytes::Bytes::copy_from_slice(region_data);
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
            let prepared = lock_prepared_files(&self.prepared_files);
            if prepared.contains(&file_index) {
                return Ok(());
            }
        }

        self.storage
            .prepare_file(path, len, self.storage.io_config().preallocation_mode)
            .await?;

        let mut prepared = lock_prepared_files(&self.prepared_files);
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
            let data = bytes::Bytes::copy_from_slice(&assembly.data);
            match self.storage.hash_sha1_retry_queue_full(data).await {
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

    async fn set_state_checked(&mut self, state: TorrentState) -> Result<(), String> {
        let (previous, row, state_event) = {
            let mut reg = self.registry.write().await;
            let Some(mut entry) = reg.get_mut(&self.info_hash_hex) else {
                return Err(format!(
                    "torrent {} is missing from the registry",
                    self.info_hash_hex
                ));
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
                return Err(error.to_string());
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
            let row = crate::engine::row_from_v1_meta(&entry, &self.meta);
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
                let updated =
                    rt_db::update_runtime_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                if !updated {
                    return Err(format!(
                        "torrent {} is missing from the database",
                        row.info_hash
                    ));
                }
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
                restore_runtime_projection(&mut entry, &previous);
            }
            warn!(
                component = "db",
                operation = "persist_torrent_state",
                torrent = %self.info_hash_hex,
                result = "error",
                error = %error,
                "failed to persist torrent state"
            );
            return Err(error);
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
        Ok(())
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
            let row = crate::engine::row_from_v1_meta(&entry, &self.meta);
            (previous, row)
        };
        let persistence = self
            .db
            // `run_batched`: the worker may commit this alongside other
            // torrents' queued progress writes in one shared transaction
            // instead of one fsync'd commit per torrent per tick — this is
            // the actual write-throughput ceiling at high torrent counts,
            // since every `TorrentTask` ticks and persists independently.
            .run_batched("persist_torrent_progress", move |tx| {
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
                restore_progress_projection(&mut entry, &previous);
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
        verified_bytes: u64,
        invalid_pieces: &RecheckInvalidPieces,
        state: &str,
        message: Option<&str>,
    ) {
        let now = unix_now();
        let done = i64::from(next_piece.min(self.piece_map.piece_count));
        let byte_offset = self.verified_byte_offset(next_piece);
        let verified_bytes = db_i64(verified_bytes);
        let job_id = job_id.to_owned();
        let state = state.to_owned();
        let message = message.map(str::to_owned);
        let invalid_piece_count = invalid_pieces.total;
        let invalid_pieces = invalid_pieces.values.clone();
        let invalid_pieces_truncated = invalid_piece_count > rt_db::MAX_JOB_INVALID_PIECES;
        let log_job_id = job_id.clone();
        let log_state = state.clone();
        let persistence = self
            .db
            .run("persist_recheck_progress", move |db| {
                let mut job = rt_db::get_job(db, &job_id).map_err(|error| error.to_string())?;
                // A cancellation/failure/completion written by the engine
                // control path wins over any progress message already in
                // flight. Without this fence, a late actor write could
                // resurrect a terminal recheck job on the next snapshot.
                if job.finished_at.is_some()
                    || matches!(
                        job.state.as_str(),
                        JOB_STATE_CANCELLED | JOB_STATE_COMPLETED | JOB_STATE_FAILED
                    )
                {
                    return Ok(());
                }
                if job.state == JOB_STATE_CANCELLING
                    && state != JOB_STATE_CANCELLED
                    && state != JOB_STATE_COMPLETED
                {
                    // The engine has durably recorded a cancellation intent.
                    // Do not let a progress or completion write resurrect
                    // the job while the actor is still unwinding the scan.
                    return Ok(());
                }
                let cancellation_completed_after_request =
                    job.state == JOB_STATE_CANCELLING && state == JOB_STATE_COMPLETED;
                let state = if cancellation_completed_after_request {
                    JOB_STATE_CANCELLED.to_owned()
                } else {
                    state
                };
                let message = if cancellation_completed_after_request {
                    Some("recheck cancellation arrived after verification completed".to_owned())
                } else {
                    message
                };
                job.state = state.clone();
                job.done = done;
                job.checkpoint = done;
                job.piece_index = Some(done);
                job.byte_offset = Some(byte_offset);
                job.verified_bytes = verified_bytes;
                job.invalid_pieces = invalid_pieces.clone();
                job.updated_at = db_i64(now);
                if matches!(
                    state.as_str(),
                    JOB_STATE_CANCELLED | JOB_STATE_COMPLETED | JOB_STATE_FAILED
                ) {
                    job.finished_at = Some(db_i64(now));
                }
                let event = rt_db::JobEventRow {
                    event_id: None,
                    job_id: job_id.clone(),
                    occurred_at: db_i64(now),
                    kind: match state.as_str() {
                        JOB_STATE_CANCELLED => "check_cancelled",
                        JOB_STATE_COMPLETED => "check_completed",
                        JOB_STATE_FAILED => "check_failed",
                        _ => "check_progress",
                    }
                    .to_owned(),
                    message,
                    payload: serde_json::json!({
                        "piece_index": job.piece_index,
                        "verified_bytes": job.verified_bytes,
                        "invalid_piece_count": invalid_piece_count,
                        "invalid_pieces_truncated": invalid_pieces_truncated,
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
        let durable_assembly_pieces = self.flush_piece_assemblies_to_disk().await;
        let (uploaded, downloaded) = self.transfer_snapshot().await;
        let mut state = FastresumeState::new_empty(
            &self.meta.info_hash,
            self.meta.pieces.len() as u32,
            ImportPolicy::RequireVerification,
        );
        for (piece, piece_state) in state.pieces.iter_mut().enumerate() {
            *piece_state = if self.picker.have_piece(piece) {
                PieceState::Valid
            } else {
                PieceState::Unknown
            };
        }
        state.partial_pieces = self
            .picker
            .partial_pieces_limited(MAX_FASTRESUME_BLOCKS_PER_PARTIAL_PIECE)
            .into_iter()
            .filter(|(piece, _)| {
                !self.piece_assemblies.contains_key(piece)
                    || durable_assembly_pieces.contains(piece)
            })
            .map(|(piece, received_blocks)| PartialPieceState {
                piece,
                received_blocks,
            })
            .collect();
        state.uploaded_bytes = uploaded;
        state.downloaded_bytes = downloaded;
        if self.dirty_pieces_since_barrier.is_overflowed() {
            // An empty watermark with clean_shutdown=false is intentional:
            // the loader treats it as unverifiable and performs a full
            // recheck instead of trusting an incomplete dirty-piece list.
            warn!(
                component = "fastresume",
                operation = "save",
                torrent = %self.info_hash_hex,
                result = "watermark_overflow",
                maximum = MAX_RUNTIME_DIRTY_PIECES,
                "dirty-piece watermark exceeded its live bound; an unclean save will require a full recheck"
            );
        } else {
            state.set_dirty_pieces_since_barrier(
                self.dirty_pieces_since_barrier.pieces.iter().copied(),
            );
        }
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

        if let Err(e) = self.fastresume.save_async(state).await {
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

    /// Make partial in-memory assemblies durable before advertising their
    /// received block indexes in fastresume. A clean fastresume record that
    /// points at bytes which only existed in RAM would make the next process
    /// trust a partial piece that the disk does not contain.
    async fn flush_piece_assemblies_to_disk(&self) -> HashSet<u32> {
        let assemblies = self
            .piece_assemblies
            .iter()
            .map(|(piece, assembly)| (*piece, assembly.received_blocks()))
            .collect::<Vec<_>>();
        let mut durable = HashSet::new();
        for (piece, blocks) in assemblies {
            let mut success = true;
            for (offset, data) in blocks {
                if let Err(error) = self
                    .write_block(&BlockEvent {
                        piece,
                        offset,
                        data,
                    })
                    .await
                {
                    warn!(
                        component = "fastresume",
                        operation = "flush_piece_assembly",
                        torrent = %self.info_hash_hex,
                        piece,
                        offset,
                        result = "error",
                        error = %error,
                        "could not flush an in-memory partial piece before fastresume save"
                    );
                    success = false;
                    break;
                }
            }
            if success {
                durable.insert(piece);
            }
        }
        durable
    }
}

fn reset_webseed_sleep(sleep: &mut Pin<Box<Sleep>>, delay: Duration) {
    sleep.as_mut().reset(tokio::time::Instant::now() + delay);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutgoingTransportPolicy {
    Auto,
    TcpOnly,
    PreferUtp,
    UtpOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerSource {
    Tracker,
    Dht,
    PeerExchange,
    Manual,
}

pub(crate) fn outgoing_transport_policy_configured() -> OutgoingTransportPolicy {
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

pub(crate) fn outgoing_transport_policy_for_peer(
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
        .filter(|file| !file.pad)
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

pub(crate) fn tracker_tiers_from_urls(urls: &[String]) -> Vec<Vec<TrackerState>> {
    let mut seen = HashSet::new();
    let trackers = urls
        .iter()
        .filter_map(|url| {
            if url.is_empty() || !seen.insert(url.clone()) {
                None
            } else {
                Some(TrackerState::new(url.clone()))
            }
        })
        .collect::<Vec<_>>();
    if trackers.is_empty() {
        Vec::new()
    } else {
        vec![trackers]
    }
}

/// Restore the small amount of tracker session state that must survive an
/// actor restart. Tracker IDs are opaque BEP 3 bytes and cannot be recovered
/// from the metainfo; losing them makes the next HTTP announce a new tracker
/// session. A persisted next deadline is restored as well so startup does
/// not create an announce storm for every resumed torrent. The query itself
/// is issued through `DbExecutor` before this pure projection runs.
pub(crate) fn restore_tracker_state_from_rows(
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

pub(crate) fn private_peer_source_allowed(
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

pub(crate) fn private_peer_port_fallback_allowed(ip: IpAddr) -> bool {
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

fn webseed_byte_range(
    piece: u32,
    piece_length: u64,
    begin: u32,
    length: u32,
) -> anyhow::Result<(u64, u64)> {
    if piece_length == 0 {
        anyhow::bail!("webseed piece length must not be zero");
    }
    if length == 0 || length > MAX_BLOCK_SIZE {
        anyhow::bail!("webseed block length {length} is invalid");
    }
    let piece_start = u64::from(piece)
        .checked_mul(piece_length)
        .ok_or_else(|| anyhow::anyhow!("webseed piece offset overflow"))?;
    let start = piece_start
        .checked_add(u64::from(begin))
        .ok_or_else(|| anyhow::anyhow!("webseed block start overflow"))?;
    let end = start
        .checked_add(u64::from(length - 1))
        .ok_or_else(|| anyhow::anyhow!("webseed block end overflow"))?;
    Ok((start, end))
}

fn validate_webseed_range_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    expected_start: u64,
    expected_end: u64,
) -> anyhow::Result<()> {
    if status != StatusCode::PARTIAL_CONTENT {
        // A 200 response may contain the complete file from byte zero even
        // though the server ignored our Range header. Accepting and
        // truncating that body would write the wrong bytes for every
        // non-zero block.
        anyhow::bail!("webseed did not honor byte range: HTTP {status}");
    }
    let content_range = headers
        .get(CONTENT_RANGE)
        .ok_or_else(|| anyhow::anyhow!("webseed 206 response is missing Content-Range"))?
        .to_str()
        .map_err(|_| anyhow::anyhow!("webseed Content-Range is not valid ASCII"))?;
    let range = content_range
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
        .and_then(|(range, _total)| range.split_once('-'));
    let Some((start, end)) = range else {
        anyhow::bail!("webseed returned malformed Content-Range: {content_range}");
    };
    let start = start.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("webseed returned malformed Content-Range: {content_range}")
    })?;
    let end = end.parse::<u64>().map_err(|_| {
        anyhow::anyhow!("webseed returned malformed Content-Range: {content_range}")
    })?;
    if start != expected_start || end != expected_end {
        anyhow::bail!(
            "webseed returned byte range {start}-{end}, expected {expected_start}-{expected_end}"
        );
    }
    Ok(())
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
    let value = Decoder::new(payload)
        .with_max_nodes(MAX_UT_PEX_BENCODE_NODES)
        .decode()?;
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

fn reconcile_peer_availability<A: PieceAvailability + ?Sized, B: PieceAvailability + ?Sized>(
    availability: &mut Availability,
    old: &A,
    new: &B,
) {
    let piece_count = availability.piece_count();
    for piece in 0..piece_count {
        match (old.has_piece(piece), new.has_piece(piece)) {
            (false, true) => availability.add_have(piece),
            (true, false) => availability.remove_have(piece),
            _ => {}
        }
    }
}

fn remove_peer_availability(availability: &mut Availability, peer_has: &PieceBitmap) {
    for piece in 0..availability.piece_count() {
        if peer_has.get(piece).unwrap_or(false) {
            availability.remove_have(piece);
        }
    }
}

fn tracker_event_after_success(current: TrackerEvent, sent: TrackerEvent) -> TrackerEvent {
    // A newer lifecycle event may have been queued while this request was in
    // flight (for example, the torrent can finish while its Started announce
    // is waiting on the tracker). Only consume the event represented by the
    // response; otherwise a successful Started response can erase a pending
    // Completed notification.
    if current == sent && matches!(sent, TrackerEvent::Started | TrackerEvent::Completed) {
        TrackerEvent::Empty
    } else {
        current
    }
}

fn tracker_event_after_incomplete_recheck(current: TrackerEvent) -> TrackerEvent {
    // A Started event may still be pending for a newly resumed session. A
    // Completed or Stopped event, however, no longer describes an active
    // incomplete torrent after the recheck and must not be sent later.
    match current {
        TrackerEvent::Started => TrackerEvent::Started,
        TrackerEvent::Empty | TrackerEvent::Completed | TrackerEvent::Stopped => {
            TrackerEvent::Empty
        }
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

fn peer_event_if_active(paused: bool, event: PeerEvent) -> Option<PeerEvent> {
    (!paused).then_some(event)
}

async fn send_peer_event(peer_event_tx: &mpsc::Sender<PeerEvent>, event: PeerEvent) -> bool {
    matches!(
        timeout(PEER_EVENT_SEND_TIMEOUT, peer_event_tx.send(event)).await,
        Ok(Ok(()))
    )
}

async fn send_choke_event(
    peer_event_tx: &mpsc::Sender<PeerEvent>,
    peer: SocketAddr,
    id: PeerId,
    choked_requests: Vec<BlockRequest>,
    outstanding: &mut Vec<OutstandingRequest>,
) -> bool {
    if send_peer_event(
        peer_event_tx,
        PeerEvent::Choked {
            peer,
            id,
            outstanding: choked_requests.clone(),
        },
    )
    .await
    {
        true
    } else {
        // The actor still owns these reservations because the Choked event
        // was not delivered. Keep them for the terminal disconnect fallback.
        outstanding.extend(choked_requests.into_iter().map(OutstandingRequest::new));
        false
    }
}

/// Terminal peer events must eventually reach the actor while its channel is
/// still open. Dropping a disconnect because the dedicated queue is full
/// strands the active peer entry, its picker reservations, and its global
/// peer permit. The peer task is owned by the torrent actor, so shutdown can
/// abort this wait if the actor itself is no longer making progress.
async fn send_peer_disconnect_event(
    peer_event_tx: &mpsc::Sender<PeerEvent>,
    event: PeerEvent,
) -> bool {
    peer_event_tx.send(event).await.is_ok()
}

/// Open a TCP connection, complete BEP 3 handshake, receive Piece messages.
async fn run_outgoing_peer_with_policy(
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    policy: OutgoingTransportPolicy,
) -> anyhow::Result<PeerLoopExit> {
    match policy {
        OutgoingTransportPolicy::Auto => {
            run_outgoing_peer(addr, peer_id, info_hash, peer_event_tx, peer_cmd_rx, upload).await
        }
        OutgoingTransportPolicy::TcpOnly => {
            run_outgoing_peer(addr, peer_id, info_hash, peer_event_tx, peer_cmd_rx, upload).await
        }
        OutgoingTransportPolicy::UtpOnly => {
            run_outgoing_utp_peer(addr, peer_id, info_hash, peer_event_tx, peer_cmd_rx, upload)
                .await
        }
        OutgoingTransportPolicy::PreferUtp => match UtpStream::connect(addr).await {
            Ok(stream) => match prepare_outgoing_utp_peer(addr, info_hash, &upload, stream).await {
                Ok(peer_io) => Ok(run_peer_loop(
                    addr,
                    peer_id,
                    peer_io,
                    peer_event_tx,
                    peer_cmd_rx,
                    upload,
                    false,
                )
                .await),
                Err(e) => {
                    debug!(
                        component = "peer",
                        operation = "setup_utp",
                        peer = %addr,
                        result = "fallback",
                        error = %e,
                        "uTP peer setup failed; falling back to TCP"
                    );
                    run_outgoing_peer(addr, peer_id, info_hash, peer_event_tx, peer_cmd_rx, upload)
                        .await
                }
            },
            Err(e) => {
                debug!(
                    component = "peer",
                    operation = "connect_utp",
                    peer = %addr,
                    result = "fallback",
                    error = %e,
                    "uTP peer path failed; falling back to TCP"
                );
                run_outgoing_peer(addr, peer_id, info_hash, peer_event_tx, peer_cmd_rx, upload)
                    .await
            }
        },
    }
}

async fn run_outgoing_peer(
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
) -> anyhow::Result<PeerLoopExit> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;

    let mut framed = Framed::with_capacity(
        stream,
        PeerCodec::with_resources(upload.resources.clone()),
        1,
    );

    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol_and_fast(),
    };
    // Send our handshake as raw bytes before the codec takes over.
    {
        use tokio::io::AsyncWriteExt;
        let inner = framed.get_mut();
        timeout(PEER_SOCKET_WRITE_TIMEOUT, inner.write_all(&our_hs.encode()))
            .await
            .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;
    }

    let (remote_supports_extension, remote_supports_fast) = {
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
        (
            remote_hs.reserved.supports_extension_protocol(),
            remote_hs.reserved.supports_fast_extension(),
        )
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
    send_have_state(
        &mut peer_io,
        &upload.have_pieces,
        &upload.resources,
        remote_supports_fast,
    )
    .await?;
    peer_io.send(Message::Interested).await?;

    Ok(run_peer_loop(
        addr,
        peer_id,
        peer_io,
        peer_event_tx,
        peer_cmd_rx,
        upload,
        false,
    )
    .await)
}

async fn run_outgoing_utp_peer(
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
) -> anyhow::Result<PeerLoopExit> {
    let stream = UtpStream::connect(addr).await?;
    run_established_utp_peer(
        addr,
        peer_id,
        info_hash,
        peer_event_tx,
        peer_cmd_rx,
        upload,
        stream,
    )
    .await
}

async fn prepare_outgoing_utp_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    upload: &UploadContext,
    mut stream: UtpStream,
) -> anyhow::Result<PeerIo> {
    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol_and_fast(),
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
    let remote_supports_fast = remote_hs.reserved.supports_fast_extension();

    let mut peer_io = PeerIo::Utp(Box::new(UtpPeerIo::new(stream, upload.resources.clone())));
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(
        &mut peer_io,
        &upload.have_pieces,
        &upload.resources,
        remote_supports_fast,
    )
    .await?;
    peer_io.send(Message::Interested).await?;
    Ok(peer_io)
}

async fn run_established_utp_peer(
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    stream: UtpStream,
) -> anyhow::Result<PeerLoopExit> {
    let peer_io = prepare_outgoing_utp_peer(addr, info_hash, &upload, stream).await?;

    Ok(run_peer_loop(
        addr,
        peer_id,
        peer_io,
        peer_event_tx,
        peer_cmd_rx,
        upload,
        false,
    )
    .await)
}

#[allow(clippy::too_many_arguments)]
async fn run_incoming_peer(
    stream: TcpStream,
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    remote_supports_extension: bool,
    remote_supports_fast: bool,
) -> anyhow::Result<PeerLoopExit> {
    stream.set_nodelay(true)?;
    let mut framed = Framed::with_capacity(
        stream,
        PeerCodec::with_resources(upload.resources.clone()),
        1,
    );

    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol_and_fast(),
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
    send_have_state(
        &mut peer_io,
        &upload.have_pieces,
        &upload.resources,
        remote_supports_fast,
    )
    .await?;
    peer_io.send(Message::Interested).await?;
    Ok(run_peer_loop(
        addr,
        peer_id,
        peer_io,
        peer_event_tx,
        peer_cmd_rx,
        upload,
        false,
    )
    .await)
}

#[allow(clippy::too_many_arguments)]
async fn run_incoming_utp_peer(
    mut stream: UtpStream,
    addr: SocketAddr,
    peer_id: PeerId,
    info_hash: [u8; 20],
    peer_event_tx: mpsc::Sender<PeerEvent>,
    peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    upload: UploadContext,
    remote_supports_extension: bool,
    remote_supports_fast: bool,
) -> anyhow::Result<PeerLoopExit> {
    let our_hs = Handshake {
        info_hash,
        peer_id: crate::peer_id::our_peer_id(),
        reserved: ExtensionFlags::with_extension_protocol_and_fast(),
    };
    timeout(
        PEER_SOCKET_WRITE_TIMEOUT,
        stream.write_all(&our_hs.encode()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("peer handshake write timed out"))??;

    let mut peer_io = PeerIo::Utp(Box::new(UtpPeerIo::new(stream, upload.resources.clone())));
    send_extension_handshake(
        &mut peer_io,
        upload.metadata.as_ref(),
        upload.is_private,
        upload.pex_enabled,
        remote_supports_extension,
    )
    .await?;
    send_have_state(
        &mut peer_io,
        &upload.have_pieces,
        &upload.resources,
        remote_supports_fast,
    )
    .await?;
    peer_io.send(Message::Interested).await?;
    Ok(run_peer_loop(
        addr,
        peer_id,
        peer_io,
        peer_event_tx,
        peer_cmd_rx,
        upload,
        false,
    )
    .await)
}

pub(crate) enum PeerIo {
    Tcp(Framed<TcpStream, PeerCodec>),
    Utp(Box<UtpPeerIo>),
}

pub(crate) struct UtpPeerIo {
    stream: UtpStream,
    decoder: UtpFrameDecoder,
    write_buffer: UtpWireBuffer,
}

impl UtpPeerIo {
    pub(crate) fn new(stream: UtpStream, resources: ResourceGovernor) -> Self {
        Self {
            stream,
            decoder: UtpFrameDecoder::new(resources),
            write_buffer: UtpWireBuffer::default(),
        }
    }
}

impl PeerIo {
    pub(crate) async fn send(&mut self, msg: Message) -> anyhow::Result<()> {
        timeout(PEER_SOCKET_WRITE_TIMEOUT, async {
            match self {
                PeerIo::Tcp(framed) => framed.send(msg).await.map_err(Into::into),
                PeerIo::Utp(io) => io.send(msg).await,
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("peer socket write timed out"))?
    }

    pub(crate) async fn next(&mut self) -> anyhow::Result<Option<Message>> {
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
        let encoded = self.write_buffer.encode(&self.decoder, &msg)?;
        self.stream.write_all(encoded).await?;
        Ok(())
    }

    async fn next(&mut self) -> anyhow::Result<Option<Message>> {
        self.decoder.next_message(&mut self.stream).await
    }
}

/// Reusable uTP write storage. Unlike TCP's `Framed` write buffer, the uTP
/// stream path previously allocated a new `Vec<u8>` for every message. Keep
/// one bounded frame buffer per connection and retain its resource lease for
/// as long as the allocation is retained.
pub(crate) struct UtpWireBuffer {
    buffer: BytesMut,
    lease: Option<MemoryLease>,
}

impl Default for UtpWireBuffer {
    fn default() -> Self {
        Self {
            buffer: BytesMut::new(),
            lease: None,
        }
    }
}

impl UtpWireBuffer {
    pub(crate) fn encode<'a>(
        &'a mut self,
        decoder: &UtpFrameDecoder,
        msg: &Message,
    ) -> anyhow::Result<&'a [u8]> {
        let encoded_len = msg
            .encoded_len()
            .ok_or_else(|| anyhow::anyhow!("peer message exceeds the wire length limit"))?;
        self.ensure_capacity(decoder, encoded_len)?;
        self.buffer.clear();
        msg.encode_into(&mut self.buffer)?;
        Ok(self.buffer.as_ref())
    }

    fn ensure_capacity(
        &mut self,
        decoder: &UtpFrameDecoder,
        required: usize,
    ) -> anyhow::Result<()> {
        if self.buffer.capacity() < required {
            // Acquire the next frame allowance before replacing the existing
            // buffer. This preserves the admission check if a peer requests a
            // larger frame, while the old allocation remains reusable when
            // the new reservation is denied.
            let mut lease = decoder.reserve_outbound_buffer(required)?;
            let old_buffer = std::mem::replace(&mut self.buffer, BytesMut::new());
            let old_lease = self.lease.take();
            drop(old_buffer);
            drop(old_lease);

            let buffer = BytesMut::with_capacity(required);
            let capacity = u64::try_from(buffer.capacity())
                .map_err(|_| anyhow::anyhow!("uTP peer write buffer capacity overflow"))?;
            if capacity > lease.bytes() && !lease.try_grow(capacity - lease.bytes()) {
                anyhow::bail!("uTP peer write buffer allocation denied");
            }
            self.buffer = buffer;
            self.lease = Some(lease);
        } else {
            let capacity = u64::try_from(self.buffer.capacity())
                .map_err(|_| anyhow::anyhow!("uTP peer write buffer capacity overflow"))?;
            match &mut self.lease {
                Some(lease) if lease.bytes() < capacity => {
                    if !lease.try_grow(capacity - lease.bytes()) {
                        anyhow::bail!("uTP peer write buffer allocation denied");
                    }
                }
                None => {
                    self.lease = Some(decoder.reserve_outbound_buffer(self.buffer.capacity())?);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Incremental length-prefix decoding for uTP's byte stream. Unlike
/// `read_exact` into a local buffer, this keeps a partial frame across a
/// cancelled `next()` future. The peer loop intentionally cancels that future
/// whenever a command, upload completion, or timer wins its `select!`.
pub(crate) struct UtpFrameDecoder {
    buffer: Vec<u8>,
    resources: ResourceGovernor,
    // The decoder retains its backing allocation across frame boundaries so
    // it can preserve a partial frame when `next()` is cancelled. Keep a
    // lease for that capacity, plus one same-sized allowance for the parsed
    // message copy made by `Message::parse` while the wire frame is handled.
    memory_leases: Vec<MemoryLease>,
    reserved_bytes: u64,
}

impl Default for UtpFrameDecoder {
    fn default() -> Self {
        Self::new(ResourceGovernor::new(Default::default()))
    }
}

impl UtpFrameDecoder {
    pub(crate) fn new(resources: ResourceGovernor) -> Self {
        Self {
            buffer: Vec::new(),
            resources,
            memory_leases: Vec::new(),
            reserved_bytes: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub(crate) fn reserve_outbound_buffer(&self, bytes: usize) -> anyhow::Result<MemoryLease> {
        let bytes = u64::try_from(bytes)
            .map_err(|_| anyhow::anyhow!("peer message memory estimate does not fit in u64"))?;
        self.resources
            .try_acquire(MemoryClass::PeerBuffer, bytes)
            .ok_or_else(|| anyhow::anyhow!("peer message allocation of {bytes} bytes denied"))
    }

    pub(crate) async fn next_message(
        &mut self,
        stream: &mut UtpStream,
    ) -> anyhow::Result<Option<Message>> {
        loop {
            if let Some(message) = self.take_frame()? {
                return Ok(Some(message));
            }
            let chunk = match stream.recv().await {
                Ok(chunk) => chunk,
                Err(rt_utp::UtpError::Closed) => {
                    if self.is_empty() {
                        return Ok(None);
                    }
                    anyhow::bail!("uTP peer closed in the middle of a message")
                }
                Err(err) => return Err(err.into()),
            };
            if chunk.is_empty() {
                if self.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("uTP peer closed in the middle of a message");
            }
            self.push(&chunk)?;
        }
    }

    fn push(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }

        // Read only the length prefix before appending the rest of an input
        // chunk. A declared 2 MiB frame must be admitted before its backing
        // allocation grows; otherwise every uTP peer could retain one legal
        // maximum-sized partial frame outside the PeerBuffer governor.
        if self.buffer.len() < 4 {
            let header_len = (4 - self.buffer.len()).min(chunk.len());
            self.reserve_capacity(self.buffer.len().saturating_add(header_len))?;
            self.buffer.extend_from_slice(&chunk[..header_len]);
            if self.buffer.len() == 4 {
                self.reserve_declared_frame()?;
            }
            if header_len < chunk.len() {
                self.append_chunk(&chunk[header_len..])?;
            }
            return Ok(());
        }

        self.reserve_declared_frame()?;
        self.append_chunk(chunk)?;
        Ok(())
    }

    fn take_frame(&mut self) -> anyhow::Result<Option<Message>> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        self.reserve_declared_frame()?;
        let len = self.frame_length();
        let frame_len = 4usize
            .checked_add(len as usize)
            .expect("bounded peer frame length must fit usize");
        if self.buffer.len() < frame_len {
            return Ok(None);
        }
        // Parse directly from the retained frame before draining it. The
        // message parser already copies the payload variants that must outlive
        // this buffer (Piece, Bitfield, and Extended); materializing a second
        // full payload Vec here made the peak roughly three frame sizes while
        // the decoder only admitted two.
        let message = Message::parse(&self.buffer[4..frame_len])?;
        self.buffer.drain(..frame_len);
        Ok(Some(message))
    }

    fn append_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
        let required = self
            .buffer
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow::anyhow!("peer frame buffer length overflow"))?;
        self.reserve_capacity(required)?;
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    fn reserve_declared_frame(&mut self) -> anyhow::Result<()> {
        let len = self.frame_length();
        if len > rt_peer_wire::message::MAX_MESSAGE_LEN {
            anyhow::bail!("peer message too large: {len}");
        }
        let frame_len = 4usize
            .checked_add(len as usize)
            .expect("bounded peer frame length must fit usize");
        self.reserve_capacity(frame_len)
    }

    fn reserve_capacity(&mut self, required: usize) -> anyhow::Result<()> {
        if required <= self.buffer.capacity() {
            return Ok(());
        }
        let required_bytes = u64::try_from(required)
            .map_err(|_| anyhow::anyhow!("peer frame buffer length does not fit in u64"))?
            .saturating_mul(2);
        if required_bytes <= self.reserved_bytes {
            self.buffer
                .try_reserve_exact(required - self.buffer.capacity())
                .map_err(|error| anyhow::anyhow!("peer frame buffer allocation failed: {error}"))?;
            return Ok(());
        }
        let additional = required_bytes.saturating_sub(self.reserved_bytes);
        let lease = self
            .resources
            .try_acquire(MemoryClass::PeerBuffer, additional)
            .ok_or_else(|| {
                anyhow::anyhow!("peer frame buffer allocation of {required_bytes} bytes denied")
            })?;
        self.buffer
            .try_reserve_exact(required - self.buffer.capacity())
            .map_err(|error| anyhow::anyhow!("peer frame buffer allocation failed: {error}"))?;
        self.memory_leases.push(lease);
        self.reserved_bytes = required_bytes;
        Ok(())
    }

    fn frame_length(&self) -> u32 {
        let header = [
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ];
        u32::from_be_bytes(header)
    }
}

#[derive(Debug)]
struct PeerLoopExit {
    result: anyhow::Result<()>,
    /// Requests that were accepted by the torrent actor but had not yet
    /// produced a Piece event when the peer loop ended. The production
    /// wrapper forwards these to the actor's dedicated terminal channel so
    /// picker reservations are released even on a socket/error path.
    outstanding: Vec<BlockRequest>,
}

async fn run_peer_loop(
    addr: SocketAddr,
    peer_id: PeerId,
    mut peer_io: PeerIo,
    peer_event_tx: mpsc::Sender<PeerEvent>,
    mut peer_cmd_rx: mpsc::Receiver<PeerCommand>,
    mut upload: UploadContext,
    // Production wrappers use the dedicated terminal channel after this
    // loop returns. The direct loop path can opt into the ordinary event
    // channel when it owns no separate terminal delivery path.
    send_terminal_event_on_peer_channel: bool,
) -> PeerLoopExit {
    let mut outstanding = Vec::<OutstandingRequest>::new();
    // BEP 10 extension ids are negotiated per peer. The id in our outgoing
    // handshake is the id this loop receives from the peer; responses must
    // use the peer's advertised id rather than our local id.
    let mut remote_ut_metadata_id = None;
    let mut download_choked = true;
    let mut upload_choked = true;
    let mut upload_limit_bytes_per_sec = upload.upload_limit_bytes_per_sec;
    let mut upload_tokens = upload_limit_bytes_per_sec.unwrap_or(u64::MAX);
    let mut upload_tokens_updated = TokioInstant::now();
    let mut timeout_tick = interval(Duration::from_secs(5));
    let mut last_activity = Instant::now();
    let mut request_window_started = Instant::now();
    let mut upload_requests_in_window = 0u32;
    // Disk reads are scheduled through the bounded mount scheduler, but the
    // peer loop must not await one read inline: a slow device otherwise
    // prevents it from processing choke/shutdown/input messages. The loop
    // owns the socket; detached read futures only return a leased block.
    let mut upload_reads = UploadReadTasks::default();
    // A peer is allowed to pipeline more requests than the detached read
    // budget. Queue only a small bounded tail and let completed reads refill
    // the worker set; saturating this queue drops the newest request instead
    // of killing an otherwise valid connection.
    let mut pending_upload_requests = VecDeque::with_capacity(MAX_QUEUED_UPLOAD_REQUESTS);
    let mut upload_request_drops = 0u64;

    let result: anyhow::Result<()> = async {
        loop {
        tokio::select! {
            cmd = peer_cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    // A dropped command sender means the owner can no
                    // longer control this peer. Do not leave the socket and
                    // its global peer permit alive until the idle timeout.
                    break;
                };
                last_activity = Instant::now();
                match cmd {
                    PeerCommand::Request(req) => {
                        if download_choked {
                            // The engine may have queued a request just
                            // before the remote choke event reached it. Do
                            // not put that request on the wire; the engine's
                            // Choked event releases its reservation.
                            continue;
                        }
                        if let Err(error) = peer_io
                            .send(Message::Request {
                            piece: req.piece,
                            begin: req.begin,
                            length: req.length,
                        })
                        .await
                        {
                            // The torrent actor reserved this block before
                            // enqueueing the command. Preserve it for the
                            // terminal disconnect event when the socket
                            // rejects the write, otherwise the picker can
                            // strand the block as permanently requested.
                            outstanding.push(OutstandingRequest::new(req));
                            return Err(error);
                        }
                        outstanding.push(OutstandingRequest::new(req));
                    }
                    PeerCommand::Have(piece) => {
                        upload.have_pieces.set(piece as usize, true);
                        peer_io.send(Message::Have(piece)).await?;
                    }
                    PeerCommand::Choke => {
                        upload.upload_control.cancel();
                        upload_choked = true;
                        pending_upload_requests.clear();
                        // A local choke invalidates reads that were admitted
                        // under the previous upload state. Abort them now so
                        // a slow disk cannot retain peer memory until the
                        // per-read timeout.
                        abort_upload_reads(&upload_reads);
                        peer_io.send(Message::Choke).await?;
                    }
                    PeerCommand::Unchoke => {
                        upload.upload_control.cancel();
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
                        upload.upload_control.cancel();
                        upload_limit_bytes_per_sec = limit;
                        upload_tokens = upload_limit_bytes_per_sec.unwrap_or(u64::MAX);
                        upload_tokens_updated = TokioInstant::now();
                        start_upload_reads(
                            &mut upload_reads,
                            &mut pending_upload_requests,
                            &upload,
                            upload_choked,
                        );
                    }
                    PeerCommand::Shutdown => {
                        upload.upload_control.cancel();
                        upload.shutdown_control.cancel();
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
                        if let Err(error) = validate_bitfield_shape(&bits, upload.have_pieces.len()) {
                            debug!(
                                component = "peer",
                                operation = "parse_bitfield",
                                peer = %addr,
                                result = "error",
                                error = %error,
                                "ignoring invalid bitfield"
                            );
                            continue;
                        }
                        // The previous upload-side bitmap remains live until
                        // the actor replaces it, so reserve the incoming map
                        // before allocating it and account for both copies
                        // during this handoff.
                        let memory_lease = match reserve_peer_bitfield_len(
                            &upload.resources,
                            upload.have_pieces.len(),
                        ) {
                            Ok(lease) => lease,
                            Err(error) => {
                                debug!(
                                    component = "peer",
                                    operation = "reserve_bitfield_memory",
                                    peer = %addr,
                                    result = "denied",
                                    error = %error,
                                    "disconnecting peer because its bitfield exceeds the memory budget"
                                );
                                break;
                            }
                        };
                        let pieces = PieceBitmap::from_validated_bitfield(
                            &bits,
                            upload.have_pieces.len(),
                        );
                        // The packed bitmap is now the event's only copy. Do
                        // not retain the wire buffer while waiting for the
                        // bounded actor mailbox.
                        drop(bits);
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Bitfield {
                                peer: addr,
                                id: peer_id,
                                pieces,
                                _memory_lease: memory_lease,
                            },
                        )
                        .await
                        {
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
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Have {
                                peer: addr,
                                id: peer_id,
                                piece,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    // BEP 6 Fast extension: HaveAll/HaveNone stand in for a
                    // full or empty Bitfield. Feed the same availability
                    // path a real Bitfield would use so downstream piece
                    // selection/interest logic does not need to know the
                    // difference.
                    Message::HaveAll => {
                        let memory_lease = match reserve_peer_bitfield_len(
                            &upload.resources,
                            upload.have_pieces.len(),
                        ) {
                            Ok(lease) => lease,
                            Err(error) => {
                                debug!(
                                    component = "peer",
                                    operation = "reserve_bitfield_memory",
                                    peer = %addr,
                                    result = "denied",
                                    error = %error,
                                    "disconnecting peer because its fast-extension have-state exceeds the memory budget"
                                );
                                break;
                            }
                        };
                        let pieces = PieceBitmap::all_true(upload.have_pieces.len());
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Bitfield {
                                peer: addr,
                                id: peer_id,
                                pieces,
                                _memory_lease: memory_lease,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Message::HaveNone => {
                        let memory_lease = match reserve_peer_bitfield_len(
                            &upload.resources,
                            upload.have_pieces.len(),
                        ) {
                            Ok(lease) => lease,
                            Err(error) => {
                                debug!(
                                    component = "peer",
                                    operation = "reserve_bitfield_memory",
                                    peer = %addr,
                                    result = "denied",
                                    error = %error,
                                    "disconnecting peer because its fast-extension have-state exceeds the memory budget"
                                );
                                break;
                            }
                        };
                        let pieces = PieceBitmap::new(upload.have_pieces.len());
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Bitfield {
                                peer: addr,
                                id: peer_id,
                                pieces,
                                _memory_lease: memory_lease,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Message::Piece { piece, begin, data } => {
                        let data_len = data.len() as u32;
                        let memory_lease = match reserve_peer_event_bytes(&upload.resources, data.len())
                        {
                            Ok(lease) => lease,
                            Err(error) => {
                                debug!(
                                    component = "peer",
                                    operation = "reserve_piece_event_memory",
                                    peer = %addr,
                                    result = "denied",
                                    error = %error,
                                    "disconnecting peer because its received block exceeds the memory budget"
                                );
                                break;
                            }
                        };
                        if !take_matching_outstanding(&mut outstanding, piece, begin, data_len) {
                            drop(memory_lease);
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
                        let control_generation = upload.shutdown_control.generation();
                        if !upload
                            .global_download
                            .acquire_or_cancelled(
                                data_len as u64,
                                &upload.shutdown_control,
                                control_generation,
                            )
                            .await
                            || upload.shutdown_control.generation() != control_generation
                        {
                            outstanding.push(OutstandingRequest::new(BlockRequest {
                                piece,
                                begin,
                                length: data_len,
                            }));
                            continue;
                        }
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Piece {
                                peer: addr,
                                id: peer_id,
                                block: BlockEvent {
                                    piece,
                                    offset: begin,
                                    // `data` is already `bytes::Bytes` (the
                                    // wire message now carries Bytes
                                    // end-to-end), so this is a move, not a
                                    // copy.
                                    data,
                                },
                                _memory_lease: Some(memory_lease),
                            },
                        )
                        .await
                        {
                            // The actor still owns this reservation because
                            // the Piece event was not delivered. Put the
                            // coordinate back so the terminal disconnect
                            // event releases it even when other requests are
                            // still in flight.
                            outstanding.push(OutstandingRequest::new(BlockRequest {
                                piece,
                                begin,
                                length: data_len,
                            }));
                            break; // torrent task gone
                        }
                    }
                    Message::Reject {
                        piece,
                        begin,
                        length,
                    } => {
                        let rejected = BlockRequest {
                            piece,
                            begin,
                            length,
                        };
                        if !take_matching_outstanding(&mut outstanding, piece, begin, length) {
                            debug!(
                                component = "peer",
                                operation = "handle_reject",
                                peer = %addr,
                                piece,
                                begin,
                                length,
                                result = "ignored",
                                reason = "unsolicited",
                                "ignoring reject for a request that is not outstanding"
                            );
                            continue;
                        }
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::RequestRejected {
                                peer: addr,
                                id: peer_id,
                                rejected,
                            },
                        )
                        .await
                        {
                            outstanding.push(OutstandingRequest::new(rejected));
                            break;
                        }
                    }
                    Message::Unchoke => {
                        download_choked = false;
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Unchoked {
                                peer: addr,
                                id: peer_id,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Message::Interested => {
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Interested {
                                peer: addr,
                                id: peer_id,
                            },
                        )
                        .await
                        {
                            break;
                        }
                    }
                    Message::NotInterested => {
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::NotInterested {
                                peer: addr,
                                id: peer_id,
                            },
                        )
                        .await
                        {
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
                        download_choked = true;
                        let choked_requests = drain_outstanding(&mut outstanding);
                        if !send_choke_event(
                            &peer_event_tx,
                            addr,
                            peer_id,
                            choked_requests,
                            &mut outstanding,
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
                                remote_ut_metadata_id = handshake.ut_metadata_id();
                                if !send_peer_event(
                                    &peer_event_tx,
                                    PeerEvent::ExtendedHandshake {
                                        peer: addr,
                                        id: peer_id,
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
                                let Some(remote_ut_metadata_id) = remote_ut_metadata_id else {
                                    debug!(
                                        component = "peer",
                                        operation = "send_ut_metadata",
                                        peer = %addr,
                                        result = "ignored",
                                        reason = "remote_extension_id_unknown",
                                        "ignoring metadata request before peer extension negotiation"
                                    );
                                    continue;
                                };
                                let response = upload
                                    .metadata
                                    .as_ref()
                                    .map(|metadata| metadata_response(piece, metadata))
                                    .unwrap_or(UtMetadataMessage::Reject { piece });
                                peer_io.send(Message::Extended {
                                    ext_id: remote_ut_metadata_id,
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
                                let memory_bytes = pex
                                    .added
                                    .capacity()
                                    .saturating_add(pex.dropped.capacity())
                                    .saturating_mul(std::mem::size_of::<SocketAddr>());
                                let memory_lease = match reserve_peer_event_bytes(
                                    &upload.resources,
                                    memory_bytes,
                                ) {
                                    Ok(lease) => lease,
                                    Err(error) => {
                                        debug!(
                                            component = "peer",
                                            operation = "reserve_pex_event_memory",
                                            peer = %addr,
                                            result = "denied",
                                            error = %error,
                                            "disconnecting peer because its peer-exchange payload exceeds the memory budget"
                                        );
                                        break;
                                    }
                                };
                                if !send_peer_event(
                                    &peer_event_tx,
                                    PeerEvent::PeerExchange {
                                        peer: addr,
                                        id: peer_id,
                                        peers: pex.added,
                                        dropped: pex.dropped,
                                        _memory_lease: memory_lease,
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
                            error = %crate::task_join_error_summary(
                                "upload block read task",
                                &error
                            ),
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
                        let control_generation = upload.upload_control.generation();
                        let budget_available = wait_for_upload_budget(
                            upload_limit_bytes_per_sec,
                            &mut upload_tokens,
                            &mut upload_tokens_updated,
                            bytes,
                            &upload.upload_control,
                            control_generation,
                        )
                        .await;
                        let budget_available = budget_available
                            && upload
                                .global_upload
                                .acquire_or_cancelled(
                                    bytes,
                                    &upload.upload_control,
                                    control_generation,
                                )
                                .await
                            && upload.upload_control.generation() == control_generation;
                        if !budget_available {
                            continue;
                        }
                        peer_io
                            .send(Message::Piece {
                                piece: request.piece,
                                begin: request.begin,
                                data: block.data,
                            })
                            .await?;
                        if !send_peer_event(
                            &peer_event_tx,
                            PeerEvent::Uploaded {
                                peer: addr,
                                id: peer_id,
                                bytes,
                            },
                        )
                        .await
                        {
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
                            id: peer_id,
                            timed_out: timed_out.clone(),
                        },
                    )
                    .await
                {
                    outstanding.extend(timed_out.into_iter().map(OutstandingRequest::new));
                    break;
                }
            }
        }
        }
        Ok(())
    }
    .await;

    // Keep this outside the result-producing block: socket, framing, and
    // event-delivery errors can leave that block through `?`, bypassing the
    // normal loop break. Dropping a JoinHandle detaches its task, so every
    // in-flight upload read must be explicitly aborted on every exit path.
    abort_upload_reads(&upload_reads);

    let outstanding = drain_outstanding(&mut outstanding);
    if send_terminal_event_on_peer_channel {
        let _ = send_peer_event(
            &peer_event_tx,
            PeerEvent::Disconnected {
                peer: addr,
                id: peer_id,
                outstanding: outstanding.clone(),
            },
        )
        .await;
    }
    PeerLoopExit {
        result,
        outstanding,
    }
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

async fn send_have_state(
    peer_io: &mut PeerIo,
    have_pieces: &PieceBitmap,
    resources: &ResourceGovernor,
    remote_supports_fast: bool,
) -> anyhow::Result<()> {
    let total = have_pieces.len();
    let have = have_pieces.count_ones();
    // BEP 6 Fast extension: a peer that has everything or nothing can skip
    // the full bitfield entirely. Only take this path with peers that
    // negotiated Fast-extension support during the handshake; peers that
    // did not must keep getting exactly today's behavior (a normal
    // Bitfield, or nothing at all when we have zero pieces).
    if remote_supports_fast {
        if total > 0 && have == total {
            peer_io.send(Message::HaveAll).await?;
            return Ok(());
        }
        if have == 0 {
            peer_io.send(Message::HaveNone).await?;
            return Ok(());
        }
    } else if have == 0 {
        return Ok(());
    }
    let _wire_memory_lease = reserve_peer_bitfield_wire_bytes(resources, have_pieces)?;
    let bitfield = have_pieces.to_bitfield();
    peer_io.send(Message::Bitfield(bitfield)).await?;
    Ok(())
}

async fn read_upload_block(
    upload: &UploadReadContext,
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
        if region.pad {
            data.resize(data.len() + region.length as usize, 0);
            continue;
        }
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
    let read_context = UploadReadContext {
        save_root: upload.save_root.clone(),
        piece_map: upload.piece_map.clone(),
        storage: upload.storage.clone(),
        resources: upload.resources.clone(),
    };
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
        let read_context_for_task = read_context.clone();
        upload_reads.push(tokio::spawn(async move {
            let result = match timeout(
                PEER_UPLOAD_READ_TIMEOUT,
                read_upload_block(
                    &read_context_for_task,
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

fn abort_upload_reads(
    upload_reads: &futures::stream::FuturesUnordered<tokio::task::JoinHandle<UploadReadResult>>,
) {
    for read in upload_reads.iter() {
        read.abort();
    }
}

async fn wait_for_upload_budget(
    limit: Option<u64>,
    tokens: &mut u64,
    tokens_updated: &mut TokioInstant,
    bytes: u64,
    cancellation: &RateLimitCancellation,
    generation: u64,
) -> bool {
    if cancellation.generation() != generation {
        return false;
    }
    let Some(limit) = limit.filter(|limit| *limit > 0) else {
        *tokens = u64::MAX;
        *tokens_updated = TokioInstant::now();
        return cancellation.generation() == generation;
    };
    let mut remaining = bytes;
    while remaining > 0 {
        if cancellation.generation() != generation {
            return false;
        }
        let now = TokioInstant::now();
        let elapsed = now.saturating_duration_since(*tokens_updated);
        *tokens_updated = now;
        let refill = (elapsed.as_secs_f64() * limit as f64).floor() as u64;
        *tokens = tokens.saturating_add(refill).min(limit);
        let requested = remaining.min(limit);
        if *tokens >= requested {
            *tokens -= requested;
            remaining -= requested;
            continue;
        }
        let missing = requested.saturating_sub(*tokens);
        tokio::select! {
            _ = sleep(Duration::from_secs_f64(missing as f64 / limit as f64).max(Duration::from_millis(1))) => {}
            _ = cancellation.wait_for_change(generation) => return false,
        }
    }
    cancellation.generation() == generation
}

fn validate_bitfield_shape(bits: &[u8], piece_count: usize) -> anyhow::Result<()> {
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
    Ok(())
}

#[cfg(test)]
fn bitfield_to_pieces(bits: &[u8], piece_count: usize) -> anyhow::Result<Vec<bool>> {
    validate_bitfield_shape(bits, piece_count)?;

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

fn super_seed_visible_pieces<A: PieceAvailability + ?Sized>(
    have_pieces: &A,
    piece_count: usize,
    peer_addr: SocketAddr,
) -> PieceBitmap {
    let available_count = (0..piece_count)
        .filter(|piece| have_pieces.has_piece(*piece))
        .count();
    let mut visible = PieceBitmap::new(piece_count);
    if available_count == 0 {
        return visible;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    peer_addr.hash(&mut hasher);
    let target = (hasher.finish() as usize) % available_count;
    if let Some(selected) = (0..piece_count)
        .filter(|piece| have_pieces.has_piece(*piece))
        .nth(target)
    {
        visible.set(selected, true);
    }
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

fn webseed_error_for_log(error: &anyhow::Error) -> String {
    rt_tracker::sanitize_tracker_message(&error.to_string())
}

#[cfg(test)]
mod tests {
    use rt_path::SafeRelPath;
    use sha1::{Digest, Sha1};

    use super::*;

    #[test]
    fn dirty_piece_watermark_overflow_is_bounded_and_fail_closed() {
        let mut tracker = DirtyPieceTracker::default();
        for piece in 0..=MAX_RUNTIME_DIRTY_PIECES {
            tracker.insert(piece as u32);
        }

        assert!(tracker.is_overflowed());
        assert_eq!(tracker.len(), (MAX_RUNTIME_DIRTY_PIECES + 1) as u64);
        assert!(tracker.pieces.is_empty());

        tracker.clear();
        tracker.insert(7);
        assert!(!tracker.is_overflowed());
        assert_eq!(tracker.len(), 1);
        assert!(tracker.pieces.contains(&7));
    }

    #[test]
    fn recheck_invalid_piece_history_is_bounded_but_counts_all() {
        let mut invalid = RecheckInvalidPieces::new(u32::MAX);
        for piece in 0..=rt_db::MAX_JOB_INVALID_PIECES {
            invalid.record(piece as u32);
        }

        assert_eq!(invalid.values.len(), rt_db::MAX_JOB_INVALID_PIECES);
        assert_eq!(invalid.total, rt_db::MAX_JOB_INVALID_PIECES + 1);
        assert_eq!(invalid.values.first(), Some(&0));
        assert_eq!(
            invalid.values.last(),
            Some(&((rt_db::MAX_JOB_INVALID_PIECES - 1) as i64))
        );
    }

    fn persist_task_row(conn: &Connection, entry: &rt_session::TorrentEntry, meta: &TorrentMetaV1) {
        let row = crate::engine::row_from_entry(entry, &TorrentMeta::V1(meta.clone()));
        rt_db::upsert(conn, &row).unwrap();
    }

    #[test]
    fn choke_state_does_not_commit_when_peer_mailbox_is_full() {
        let (tx, mut rx) = mpsc::channel(1);
        let control = RateLimitCancellation::default();
        tx.try_send(PeerCommand::Shutdown)
            .expect("fill the test peer mailbox");

        let (state, failed) =
            TorrentTask::try_apply_choke_decision(&tx, &control, false, ChokeDecision::Choke);
        assert!(!state, "failed delivery must retain the previous state");
        assert!(failed);

        let _ = rx.try_recv();
        let generation = control.generation();
        let (state, failed) =
            TorrentTask::try_apply_choke_decision(&tx, &control, false, ChokeDecision::Choke);
        assert!(state);
        assert!(!failed);
        assert_ne!(control.generation(), generation);
    }

    #[tokio::test]
    async fn shutdown_rejects_queued_torrent_command_replies() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(2);
        let (reply, response) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause { reply: Some(reply) })
            .await
            .unwrap();
        drop(cmd_tx);
        let mut pending_command = None;

        reject_pending_torrent_commands(&mut cmd_rx, &mut pending_command);

        assert_eq!(
            response.await.unwrap(),
            Err("torrent task is shutting down".to_owned())
        );
    }

    #[tokio::test]
    async fn closed_command_channel_rearms_interrupted_stopped_announce() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [26; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "closed-command.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("closed-command.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let db = Arc::new(Mutex::new(conn));
        let (_task_cmd_tx, task_cmd_rx) = mpsc::channel(1);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            Arc::clone(&registry),
            DbExecutor::direct(Arc::clone(&db)),
            ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            task_cmd_rx,
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
            None,
        )
        .await;

        // Hold the registry write lock so announce_stopped is suspended after
        // consuming its flag. Closing the command channel then exercises the
        // interruption branch rather than the normal announce-complete path.
        task.stopped_announced = false;
        let _registry_guard = registry.write().await;
        drop(cmd_tx);
        let mut pending_command = None;
        await_stopped_announce_or_queue_command(&mut task, &mut cmd_rx, &mut pending_command).await;

        assert!(!task.stopped_announced);
        assert!(pending_command.is_none());
    }

    #[test]
    fn merged_peer_request_views_are_deduplicated() {
        let requests = vec![BlockRequest {
            piece: 2,
            begin: 0,
            length: MAX_BLOCK_SIZE,
        }];
        let merged = TorrentTask::merge_unique_block_requests(
            requests,
            vec![
                BlockRequest {
                    piece: 2,
                    begin: 0,
                    length: MAX_BLOCK_SIZE,
                },
                BlockRequest {
                    piece: 3,
                    begin: 0,
                    length: MAX_BLOCK_SIZE,
                },
            ],
        );

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].piece, 2);
        assert_eq!(merged[1].piece, 3);
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

    #[test]
    fn utp_frame_decoder_preserves_fragmented_and_coalesced_messages() {
        let wire = [
            Message::Have(7).encode(),
            Message::KeepAlive.encode(),
            Message::Unchoke.encode(),
        ]
        .concat();
        let expected = vec![Message::Have(7), Message::KeepAlive, Message::Unchoke];
        let mut decoder = UtpFrameDecoder::default();
        let mut frames = Vec::new();

        for chunk in wire.chunks(2) {
            decoder.push(chunk).unwrap();
            while let Some(message) = decoder.take_frame().unwrap() {
                frames.push(message);
            }
        }

        assert_eq!(frames, expected);
        assert!(decoder.is_empty());
    }

    #[test]
    fn utp_frame_decoder_rejects_oversized_length_before_buffer_growth() {
        let mut decoder = UtpFrameDecoder::default();
        let oversized = (rt_peer_wire::message::MAX_MESSAGE_LEN + 1).to_be_bytes();

        assert!(decoder.push(&oversized).is_err());
        assert_eq!(decoder.buffer.len(), 4);
    }

    #[test]
    fn utp_frame_decoder_admits_partial_frame_before_buffer_growth() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::PeerBuffer as usize] = 64;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 64,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let mut decoder = UtpFrameDecoder::new(governor.clone());

        // The declared frame needs 2 * (4-byte prefix + 32-byte payload) in
        // the decoder's peak allowance. Reject it after retaining only the
        // prefix; the old decoder would accept the prefix and grow to the
        // full frame without consulting the peer-buffer governor.
        assert!(decoder.push(&32u32.to_be_bytes()).is_err());
        assert_eq!(decoder.buffer.len(), 4);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            8
        );
        drop(decoder);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
    }

    #[test]
    fn utp_wire_buffer_reuses_and_releases_its_memory_lease() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::PeerBuffer as usize] = 128;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 128,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let decoder = UtpFrameDecoder::new(governor.clone());
        let mut buffer = UtpWireBuffer::default();

        let first = buffer.encode(&decoder, &Message::Have(7)).unwrap();
        assert_eq!(first, Message::Have(7).encode());
        let held = governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes;
        assert_eq!(held, buffer.lease.as_ref().unwrap().bytes());

        let second = buffer.encode(&decoder, &Message::KeepAlive).unwrap();
        assert_eq!(second, [0, 0, 0, 0]);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            held
        );

        drop(buffer);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
    }

    #[test]
    fn utp_wire_buffer_keeps_old_allocation_when_growth_is_denied() {
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::PeerBuffer as usize] = 4;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: 4,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });
        let decoder = UtpFrameDecoder::new(governor.clone());
        let mut buffer = UtpWireBuffer::default();

        buffer.encode(&decoder, &Message::KeepAlive).unwrap();
        assert!(buffer
            .encode(
                &decoder,
                &Message::Piece {
                    piece: 0,
                    begin: 0,
                    data: vec![0; 32].into(),
                }
            )
            .is_err());
        assert_eq!(buffer.buffer.len(), 4);
        assert_eq!(buffer.buffer.capacity(), 4);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            4
        );

        drop(buffer);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
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
        let durable_row = crate::engine::row_from_entry(&entry, &TorrentMeta::V1(meta.clone()));
        rt_db::upsert(&conn, &durable_row).unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
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
            None,
        )
        .await;

        task.update_transfer(4, false).await;
        task.update_transfer(2, true).await;
        assert!(task.transfer_stats_dirty);
        assert_eq!(
            rt_db::get(&db.lock().unwrap(), &info_hash)
                .unwrap()
                .downloaded,
            0
        );

        task.persist_progress().await;

        assert!(!task.transfer_stats_dirty);
        let row = rt_db::get(&db.lock().unwrap(), &info_hash).unwrap();
        assert_eq!(row.uploaded, 2);
        assert_eq!(row.downloaded, 4);
    }

    #[tokio::test]
    async fn applying_tracker_urls_repairs_stale_detail_projection() {
        let temp = tempfile::tempdir().unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let old_tracker = "http://old-tracker.example/announce".to_owned();
        let new_tracker = "http://new-tracker.example/announce".to_owned();
        let meta = TorrentMetaV1 {
            info_hash: [21; 20],
            announce: Some(old_tracker.clone()),
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "tracker-race.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("tracker-race.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 4;
        entry.transition(TorrentState::Paused).unwrap();
        let mut row = crate::engine::row_from_entry(&entry, &TorrentMeta::V1(meta.clone()));
        // Model the engine transaction having updated the compact override
        // while an older tracker-result snapshot still owns the detail rows.
        row.trackers = vec![new_tracker.clone()];
        rt_db::upsert(&conn, &row).unwrap();
        rt_db::replace_torrent_trackers(
            &mut conn,
            &info_hash,
            &[rt_db::TorrentTrackerRow {
                info_hash: info_hash.clone(),
                tracker_index: 0,
                tier: 0,
                url: old_tracker,
                tracker_id: Some(vec![0xabu8]),
                status: "error".to_owned(),
                last_announce_at: Some(1),
                next_announce_at: Some(2),
                last_success_at: None,
                failure_reason: Some("stale tracker result".to_owned()),
                warning_message: None,
                seeders: None,
                leechers: None,
                completed: None,
                uploaded: 0,
                downloaded: 0,
                left_bytes: 4,
            }],
        )
        .unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Paused,
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
            None,
        )
        .await;

        task.apply_tracker_urls(vec![new_tracker.clone()])
            .await
            .unwrap();

        let trackers = rt_db::list_torrent_trackers(&db.lock().unwrap(), &info_hash).unwrap();
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0].url, new_tracker);
        assert_eq!(trackers[0].status, "never_announced");
        assert_eq!(trackers[0].tracker_id, None);
    }

    #[test]
    fn poisoned_prepared_file_registry_is_cleared_and_rebuilt() {
        let prepared = Arc::new(Mutex::new(HashSet::from([3_u32])));
        let poisoner = Arc::clone(&prepared);
        assert!(std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the derived prepared-file registry");
        })
        .join()
        .is_err());
        assert!(prepared.is_poisoned());

        assert!(lock_prepared_files(&prepared).is_empty());
        assert!(!prepared.is_poisoned());
    }

    #[tokio::test]
    async fn persisted_file_paths_are_applied_to_runtime_and_piece_map() {
        let temp = tempfile::tempdir().unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [19; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "renamed.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]; 2],
            files: vec![
                rt_metainfo::TorrentFileV1 {
                    index: 0,
                    length: 4,
                    path: rt_path::SafeRelPath::from_name("old-a.bin", false).unwrap(),
                    offset: 0,
                    pad: false,
                },
                rt_metainfo::TorrentFileV1 {
                    index: 1,
                    length: 4,
                    path: rt_path::SafeRelPath::from_name("old-b.bin", false).unwrap(),
                    offset: 4,
                    pad: false,
                },
            ],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 8;
        entry.amount_left = 8;
        entry.transition(TorrentState::Paused).unwrap();
        persist_task_row(&conn, &entry, &meta);
        rt_db::replace_torrent_files(
            &mut conn,
            &info_hash,
            &[
                rt_db::TorrentFileRow {
                    info_hash: info_hash.clone(),
                    file_index: 0,
                    path: "persisted/a.bin".into(),
                    length: 4,
                    offset: 0,
                    priority: 1,
                    wanted: true,
                    completed_bytes: 0,
                },
                rt_db::TorrentFileRow {
                    info_hash: info_hash.clone(),
                    file_index: 1,
                    path: "persisted/b.bin".into(),
                    length: 4,
                    offset: 4,
                    priority: 1,
                    wanted: true,
                    completed_bytes: 0,
                },
            ],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Paused,
            Arc::new(RwLock::new(SessionRegistry::new())),
            DbExecutor::direct(Arc::clone(&db)),
            resources.clone(),
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
            None,
        )
        .await;

        assert!(task._file_policy_memory_lease.is_some());
        assert!(resources.snapshot().classes[MemoryClass::Metadata as usize].used_bytes > 0);
        assert_eq!(task.meta.files[0].path.as_display(), "persisted/a.bin");
        assert_eq!(task.meta.files[1].path.as_display(), "persisted/b.bin");
        assert_eq!(
            task.piece_map
                .piece_to_file_regions(0)
                .unwrap()
                .first()
                .unwrap()
                .path
                .as_display(),
            "persisted/a.bin"
        );

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE torrent_files SET path = 'runtime/a.bin' WHERE info_hash = ?1 AND file_index = 0",
                [&info_hash],
            )
            .unwrap();
        }
        task.picker.mark_have(0);
        task.picker.mark_have(1);
        assert!(task.picker.is_complete());
        task.prepared_files.lock().unwrap().insert(0);
        task.apply_file_policy_from_db().await.unwrap();
        assert_eq!(task.meta.files[0].path.as_display(), "runtime/a.bin");
        assert!(!task.picker.is_complete());
        assert!(!task.picker.have_piece(0));
        assert!(task.picker.have_piece(1));
        assert_eq!(
            task.piece_map
                .piece_to_file_regions(0)
                .unwrap()
                .first()
                .unwrap()
                .path
                .as_display(),
            "runtime/a.bin"
        );
        assert!(task.prepared_files.lock().unwrap().is_empty());

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "DELETE FROM torrent_files WHERE info_hash = ?1",
                [&info_hash],
            )
            .unwrap();
        }
        task.apply_file_policy_from_db().await.unwrap();
        assert_eq!(task.meta.files[0].path.as_display(), "old-a.bin");
        assert!(task._file_policy_memory_lease.is_none());
        assert_eq!(
            resources.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
    }

    #[tokio::test]
    async fn failed_block_write_releases_picker_request() {
        let temp = tempfile::tempdir().unwrap();
        let save_root = temp.path().join("not-a-directory");
        std::fs::write(&save_root, b"file").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = 2 * 1024 * 1024;
        let meta = TorrentMetaV1 {
            info_hash: [20; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "write-failure.bin".into(),
            piece_length,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("payload.bin", false).unwrap(),
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
            save_root.to_string_lossy().into_owned(),
        );
        entry.total_length = piece_length;
        entry.amount_left = piece_length;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            save_root,
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        let peer_has = PieceBitmap::from_bools(&[true]);
        task.picker.availability.add_have(0);
        let request = task.picker.pick(&peer_has).unwrap();
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: bytes::Bytes::from(vec![0; request.length as usize]),
        })
        .await;

        let retry = task
            .picker
            .pick(&peer_has)
            .expect("a failed disk write must make the block requestable again");
        assert_eq!(retry, request);
        assert_eq!(
            rt_db::get(&db.lock().unwrap(), &info_hash)
                .unwrap()
                .downloaded,
            0
        );
    }

    #[tokio::test]
    async fn storage_move_resume_reports_durable_state_failure() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [17; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "storage-resume.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("storage-resume.bin", false).unwrap(),
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
        entry.transition(TorrentState::Paused).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_storage_resume_checking
             BEFORE UPDATE OF state ON torrents
             WHEN NEW.state = 'checking'
             BEGIN SELECT RAISE(ABORT, 'injected storage resume failure'); END;",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Paused,
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
            None,
        )
        .await;
        let handle = tokio::spawn(task.run());
        let (reply, response) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::ResumeAfterStorageMove {
                new_save_root: None,
                resume_paused: false,
                reply,
            })
            .await
            .unwrap();
        let result = timeout(Duration::from_secs(2), response)
            .await
            .expect("storage resume did not acknowledge")
            .unwrap();
        assert!(result
            .expect_err("storage resume unexpectedly succeeded")
            .contains("injected storage resume failure"));
        assert!(taskless_state_is_paused(&registry, &info_hash).await);

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("storage resume test task did not shut down")
            .unwrap();
    }

    async fn taskless_state_is_paused(
        registry: &Arc<RwLock<SessionRegistry>>,
        info_hash: &str,
    ) -> bool {
        registry
            .read()
            .await
            .get(info_hash)
            .is_some_and(|entry| entry.state == TorrentState::Paused)
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
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
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
            None,
        )
        .await;

        task.picker.mark_have(0);
        task.seed_ratio_limit = Some(0.5);
        task.update_transfer(4, false).await;
        task.update_transfer(2, true).await;
        task.set_state_checked(TorrentState::Seeding).await.unwrap();
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

    #[tokio::test]
    async fn recheck_preserves_a_paused_torrent_lifecycle_state() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [12; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "paused.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("paused.bin", false).unwrap(),
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
        entry.transition(TorrentState::Paused).unwrap();
        persist_task_row(&conn, &entry, &meta);
        let job_id = "queued-recheck".to_owned();
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: job_id.clone(),
                kind: "recheck_torrent".to_owned(),
                state: JOB_STATE_RUNNING.to_owned(),
                dry_run: false,
                affected_torrents: vec![info_hash.clone()],
                total: 1,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: Some(0),
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: 1,
                started_at: Some(1),
                updated_at: 1,
                finished_at: None,
            },
        )
        .unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Paused,
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
            None,
        )
        .await;

        // The task is already doing its implicit paused-torrent scan. The
        // explicit command must attach the durable job to that scan rather
        // than being discarded, and an unrelated stale cancellation must not
        // stop it.
        cmd_tx
            .send(TorrentCmd::Recheck {
                job_id: Some(job_id.clone()),
            })
            .await
            .unwrap();
        cmd_tx
            .send(TorrentCmd::CancelJob {
                job_id: "stale-recheck".to_owned(),
            })
            .await
            .unwrap();

        assert!(matches!(
            task.run_recheck(None).await,
            RecheckOutcome::Complete
        ));
        assert!(task.paused);
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Paused
        );
        assert_eq!(
            rt_db::get(&db.lock().unwrap(), &info_hash).unwrap().state,
            TorrentState::Paused.as_str()
        );
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), &job_id).unwrap().state,
            JOB_STATE_COMPLETED
        );
    }

    #[tokio::test]
    async fn recheck_pause_persistence_failure_keeps_verification_running() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [15; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "pause-failure.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("pause-failure.bin", false).unwrap(),
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
        rt_db::upsert(
            &conn,
            &crate::engine::row_from_entry(&entry, &rt_metainfo::TorrentMeta::V1(meta.clone())),
        )
        .unwrap();
        registry.write().await.add(entry).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_paused_state
             BEFORE UPDATE OF state ON torrents
             WHEN NEW.state = 'paused'
             BEGIN SELECT RAISE(ABORT, 'injected paused state failure'); END;",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
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
            None,
        )
        .await;
        let (pause_reply, pause_result) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();

        assert!(matches!(
            task.run_recheck(None).await,
            RecheckOutcome::Complete
        ));
        assert!(pause_result.await.unwrap().is_err());
        assert!(!task.paused);
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Downloading
        );
        assert_eq!(
            rt_db::get(&db.lock().unwrap(), &info_hash).unwrap().state,
            TorrentState::Downloading.as_str()
        );
    }

    #[tokio::test]
    async fn cancelled_recheck_preserves_a_stopped_torrent_lifecycle_state() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [13; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "stopped.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("stopped.bin", false).unwrap(),
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
        persist_task_row(&conn, &entry, &meta);
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: "stopped-recheck".to_owned(),
                kind: "recheck_torrent".to_owned(),
                state: JOB_STATE_CANCELLING.to_owned(),
                dry_run: false,
                affected_torrents: vec![info_hash.clone()],
                total: 1,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: Some(0),
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: 1,
                started_at: Some(1),
                updated_at: 1,
                finished_at: None,
            },
        )
        .unwrap();
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Stopped,
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
            None,
        )
        .await;
        cmd_tx
            .send(TorrentCmd::CancelJob {
                job_id: "stopped-recheck".to_owned(),
            })
            .await
            .unwrap();

        assert!(matches!(
            task.run_recheck(Some("stopped-recheck".to_owned())).await,
            RecheckOutcome::Cancelled
        ));
        assert!(task.paused);
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Stopped
        );
        assert_eq!(
            rt_db::get(&db.lock().unwrap(), &info_hash).unwrap().state,
            TorrentState::Stopped.as_str()
        );
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), "stopped-recheck")
                .unwrap()
                .state,
            JOB_STATE_CANCELLED
        );
    }

    #[tokio::test]
    async fn stopped_dormant_promotion_does_not_become_paused_before_command() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [14; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "staged.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("staged.bin", false).unwrap(),
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
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Stopped,
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
            None,
        )
        .await;
        let handle = tokio::spawn(task.run());

        timeout(Duration::from_secs(1), async {
            loop {
                let state = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM torrents WHERE info_hash = ?1",
                        [&info_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if state.as_deref() == Some(TorrentState::Stopped.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("stopped promotion did not persist its staged state");
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Stopped
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("staged torrent task did not shut down")
            .unwrap();
    }

    #[tokio::test]
    async fn queued_dormant_promotion_preserves_queue_before_command() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [16; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "queued.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("queued.bin", false).unwrap(),
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
        entry.state = TorrentState::Queued;
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Queued,
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
            None,
        )
        .await;
        let handle = tokio::spawn(task.run());

        timeout(Duration::from_secs(1), async {
            loop {
                let state = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM torrents WHERE info_hash = ?1",
                        [&info_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if state.as_deref() == Some(TorrentState::Queued.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("queued promotion did not preserve its staged state");
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Queued
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("queued promotion task did not shut down")
            .unwrap();
    }

    #[tokio::test]
    async fn checking_restore_rechecks_even_when_fastresume_claims_complete() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [23; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "checking-restore.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("checking-restore.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let mut fastresume =
            FastresumeState::new_empty(&meta.info_hash, 1, ImportPolicy::RequireVerification);
        fastresume.clean_shutdown = true;
        fastresume.pieces = vec![PieceState::Valid];
        FastresumeStore::new(temp.path().join("fastresume"))
            .save(&fastresume)
            .unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 0;
        // This is the state persisted when a process stops after admitting a
        // recheck job but before the scan completes. It must take precedence
        // over an older fastresume bitmap.
        entry.state = TorrentState::Checking;
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Checking,
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
            None,
        )
        .await;
        let handle = tokio::spawn(task.run());

        timeout(Duration::from_secs(2), async {
            loop {
                let state = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM torrents WHERE info_hash = ?1",
                        [&info_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if state.as_deref() == Some(TorrentState::Downloading.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("checking restore did not verify the current payload");
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Downloading
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("checking restore task did not shut down")
            .unwrap();
    }

    #[tokio::test]
    async fn error_dormant_promotion_accepts_resume_before_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [15; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "error-recovery.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("error-recovery.bin", false).unwrap(),
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
        entry.set_error("previous task failure");
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(2);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Error,
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
            None,
        )
        .await;
        let handle = tokio::spawn(task.run());
        let (reply, response) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Resume { reply: Some(reply) })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(2), response)
                .await
                .expect("error promotion did not receive a resume acknowledgement")
                .unwrap(),
            Ok(())
        );
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Checking
        );

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), handle)
            .await
            .expect("error recovery task did not shut down")
            .unwrap();
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
    fn peer_bitfield_reservation_is_bounded_by_governor() {
        let mut bits = [0xff; 9];
        bits[8] = 0x80;
        let pieces = PieceBitmap::from_bitfield(&bits, 65).unwrap();
        let bytes = pieces.memory_bytes();
        assert_eq!(bytes, 2 * std::mem::size_of::<u64>() as u64);

        let mut class_caps_bytes = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::PeerBuffer as usize] = bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_peer_bitfield_bytes(&governor, &pieces).unwrap();
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            bytes
        );
        assert!(reserve_peer_bitfield_bytes(&governor, &pieces).is_err());
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
    }

    #[test]
    fn peer_bitfield_wire_reservation_covers_transient_copies() {
        let pieces = PieceBitmap::from_bools(&[true; 65]);
        let wire_bytes = 3 * pieces.len().div_ceil(8) as u64 + 5;
        let mut class_caps_bytes = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::PeerBuffer as usize] = wire_bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: wire_bytes,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_peer_bitfield_wire_bytes(&governor, &pieces).unwrap();
        assert_eq!(lease.bytes(), wire_bytes);
        assert!(reserve_peer_bitfield_wire_bytes(&governor, &pieces).is_err());
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PeerBuffer as usize].used_bytes,
            0
        );
    }

    #[test]
    fn piece_assembly_reservation_is_owned_until_assembly_drop() {
        let bytes = 4 * 1024usize;
        let mut class_caps_bytes = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::PieceAssembly as usize] = bytes as u64;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes as u64,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_piece_assembly_bytes(&governor, bytes).unwrap();
        let assembly = PieceAssembly::with_memory_lease(bytes, lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PieceAssembly as usize].used_bytes,
            bytes as u64
        );
        assert!(reserve_piece_assembly_bytes(&governor, 1).is_err());
        drop(assembly);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::PieceAssembly as usize].used_bytes,
            0
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
    fn download_budget_accepts_a_protocol_block_below_the_rate_limit() {
        let limit = 1_000;
        let mut tokens = download_bucket_capacity(limit);

        assert!(consume_download_tokens(
            &mut tokens,
            limit,
            u64::from(MAX_BLOCK_SIZE)
        ));
        assert_eq!(tokens, 0);
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
        let visible = super_seed_visible_pieces(&[true, true, false, true], 4, addr);

        assert_eq!(visible.count_ones(), 1);
        assert_eq!(visible.get(2), Some(false));
    }

    #[test]
    fn super_seed_visible_pieces_handles_empty_have_set() {
        let addr = "127.0.0.1:6881".parse().unwrap();

        assert_eq!(
            super_seed_visible_pieces(&[false, false, false], 3, addr),
            PieceBitmap::new(3)
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
    fn webseed_range_response_rejects_server_that_ignores_range() {
        let headers = reqwest::header::HeaderMap::new();

        let error =
            validate_webseed_range_response(StatusCode::OK, &headers, 16_384, 32_767).unwrap_err();

        assert!(error.to_string().contains("did not honor byte range"));
    }

    #[test]
    fn webseed_byte_range_rejects_invalid_or_overflowing_coordinates() {
        assert_eq!(
            webseed_byte_range(2, 16_384, 4, 8).unwrap(),
            (32_772, 32_779)
        );
        assert!(webseed_byte_range(0, 16_384, 0, 0).is_err());
        assert!(webseed_byte_range(u32::MAX, u64::MAX, 0, 1).is_err());
        assert!(webseed_byte_range(0, 16_384, 0, MAX_BLOCK_SIZE + 1).is_err());
    }

    #[test]
    fn webseed_error_log_text_redacts_credentials_and_controls() {
        let error = anyhow::anyhow!(
            "request failed for https://seed-user:seed-secret@cdn.example/private/key?token=query-secret#fragment\n\u{001b}[31mspoof"
        );

        let message = webseed_error_for_log(&error);

        assert!(message.contains("https://cdn.example/"));
        assert!(message.contains("spoof"));
        assert!(!message.chars().any(char::is_control));
        for secret in [
            "seed-user",
            "seed-secret",
            "private",
            "query-secret",
            "fragment",
        ] {
            assert!(!message.contains(secret), "{message}");
        }
    }

    #[test]
    fn webseed_range_response_requires_the_requested_content_range() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            CONTENT_RANGE,
            reqwest::header::HeaderValue::from_static("bytes 16384-32767/65536"),
        );

        assert!(validate_webseed_range_response(
            StatusCode::PARTIAL_CONTENT,
            &headers,
            16_384,
            32_767,
        )
        .is_ok());
        assert!(
            validate_webseed_range_response(StatusCode::PARTIAL_CONTENT, &headers, 0, 16_383,)
                .is_err()
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
    fn rejects_ut_pex_flat_bencode_node_bombs() {
        let mut payload = b"l".to_vec();
        for _ in 0..=MAX_UT_PEX_BENCODE_NODES {
            payload.extend_from_slice(b"i1e");
        }
        payload.push(b'e');

        let error = parse_ut_pex_peers(&payload).unwrap_err();
        assert!(error.to_string().contains("node limit exceeded"));
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
        // The missing data file and synthetic pad entry are both omitted
        // without suppressing a hint for the present payload file.

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
                rt_metainfo::TorrentFileV1 {
                    index: 2,
                    length: 11,
                    path: rt_path::SafeRelPath::from_components(&[".pad", "11"], false).unwrap(),
                    offset: 5,
                    pad: true,
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
        assert_eq!(assembly.received_blocks().len(), 1);
        assert_eq!(assembly.received_blocks()[0].0, 0);
        assert_eq!(
            assembly.received_blocks()[0].1.len(),
            MAX_BLOCK_SIZE as usize
        );
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

    #[tokio::test]
    async fn fastresume_flushes_partial_in_memory_assembly_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = u64::from(MAX_BLOCK_SIZE) * 2;
        let meta = TorrentMetaV1 {
            info_hash: [21; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "partial.bin".into(),
            piece_length,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("partial.bin", false).unwrap(),
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
        entry.total_length = piece_length;
        entry.amount_left = piece_length;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        task.picker.availability.add_have(0);
        let peer_has = PieceBitmap::from_bools(&[true]);
        let request = task.picker.pick(&peer_has).unwrap();
        let payload = bytes::Bytes::from(vec![0xA5; request.length as usize]);
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: payload.clone(),
        })
        .await;
        assert!(task.piece_assemblies.contains_key(&request.piece));

        task.save_fastresume(false).await;

        let on_disk = std::fs::read(temp.path().join("partial.bin")).unwrap();
        assert_eq!(on_disk.len(), piece_length as usize);
        assert_eq!(&on_disk[..request.length as usize], payload.as_ref());
        assert!(on_disk[request.length as usize..]
            .iter()
            .all(|byte| *byte == 0));
        let state = task.fastresume.load(&info_hash).unwrap();
        assert_eq!(
            state.partial_pieces,
            vec![PartialPieceState {
                piece: request.piece,
                received_blocks: vec![0],
            }]
        );
    }

    #[tokio::test]
    async fn hash_failure_does_not_persist_piece_as_valid() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [22; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "invalid-piece.bin".into(),
            piece_length: 4,
            // The received payload is four 0xA5 bytes, so this deliberately
            // cannot validate against the all-zero expected hash.
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("invalid-piece.bin", false).unwrap(),
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
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        task.picker.availability.add_have(0);
        let request = task.picker.pick(&PieceBitmap::from_bools(&[true])).unwrap();
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: bytes::Bytes::from_static(b"\xA5\xA5\xA5\xA5"),
        })
        .await;

        let state = task.fastresume.load(&info_hash).unwrap();
        assert_eq!(state.pieces, vec![PieceState::Unknown]);
        assert!(state.partial_pieces.is_empty());
        assert!(!task.picker.have_piece(0));
    }

    #[tokio::test]
    async fn pause_flushes_partial_assembly_before_clearing_peers() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = u64::from(MAX_BLOCK_SIZE) * 2;
        let meta = TorrentMetaV1 {
            info_hash: [24; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "pause-partial.bin".into(),
            piece_length,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("pause-partial.bin", false).unwrap(),
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
        entry.total_length = piece_length;
        entry.amount_left = piece_length;
        entry.transition(TorrentState::Paused).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(2);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            true,
            TorrentState::Paused,
            registry,
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
            None,
        )
        .await;

        task.picker.availability.add_have(0);
        let request = task.picker.pick(&PieceBitmap::from_bools(&[true])).unwrap();
        let payload = bytes::Bytes::from(vec![0xA5; request.length as usize]);
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: payload.clone(),
        })
        .await;
        assert!(task.piece_assemblies.contains_key(&request.piece));

        let actor = tokio::spawn(task.run());
        let (pause_reply, pause_result) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(2), pause_result)
                .await
                .expect("pause reply was not delivered")
                .unwrap(),
            Ok(())
        );

        let on_disk = std::fs::read(temp.path().join("pause-partial.bin")).unwrap();
        assert_eq!(on_disk.len(), piece_length as usize);
        assert_eq!(&on_disk[..request.length as usize], payload.as_ref());

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(2), actor)
            .await
            .expect("paused task did not shut down")
            .unwrap();
    }

    #[tokio::test]
    async fn recheck_flushes_partial_assembly_before_clearing_peers() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = u64::from(MAX_BLOCK_SIZE) * 2;
        let mut complete_piece = vec![0xA5; MAX_BLOCK_SIZE as usize];
        complete_piece.extend(vec![0x5A; MAX_BLOCK_SIZE as usize]);
        let piece_hash: [u8; 20] = Sha1::digest(&complete_piece).into();
        let meta = TorrentMetaV1 {
            info_hash: [23; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "recheck-partial.bin".into(),
            piece_length,
            pieces: vec![piece_hash],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("recheck-partial.bin", false).unwrap(),
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
        entry.total_length = piece_length;
        entry.amount_left = piece_length;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        task.picker.availability.add_have(0);
        let request = task.picker.pick(&PieceBitmap::from_bools(&[true])).unwrap();
        let payload = bytes::Bytes::from(vec![0xA5; request.length as usize]);
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: payload.clone(),
        })
        .await;
        assert!(task.piece_assemblies.contains_key(&request.piece));

        assert!(matches!(
            task.run_recheck(None).await,
            RecheckOutcome::Complete
        ));
        let on_disk = std::fs::read(temp.path().join("recheck-partial.bin")).unwrap();
        assert_eq!(&on_disk[..request.length as usize], payload.as_ref());
    }

    #[tokio::test]
    async fn restored_partial_piece_writes_new_blocks_directly() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = u64::from(MAX_BLOCK_SIZE) * 2;
        let first = vec![0x11; MAX_BLOCK_SIZE as usize];
        let second = vec![0x22; MAX_BLOCK_SIZE as usize];
        let mut piece_data = first.clone();
        piece_data.extend_from_slice(&second);
        let piece_hash: [u8; 20] = Sha1::digest(&piece_data).into();
        let meta = TorrentMetaV1 {
            info_hash: [22; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "restored-partial.bin".into(),
            piece_length,
            pieces: vec![piece_hash],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("restored-partial.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let file_path = temp.path().join("restored-partial.bin");
        let mut initial_data = first.clone();
        initial_data.extend(vec![0; second.len()]);
        std::fs::write(&file_path, initial_data).unwrap();
        let info_hash = hex::encode(meta.info_hash);
        let mut fastresume =
            FastresumeState::new_empty(&meta.info_hash, 1, ImportPolicy::RequireVerification);
        fastresume.clean_shutdown = true;
        fastresume.partial_pieces = vec![PartialPieceState {
            piece: 0,
            received_blocks: vec![0],
        }];
        fastresume.file_hints = collect_file_hints(temp.path(), &meta);
        FastresumeStore::new(temp.path().join("fastresume"))
            .save(&fastresume)
            .unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = piece_length;
        entry.amount_left = piece_length;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        registry.write().await.add(entry).unwrap();

        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        assert!(task.restore_fastresume().await);
        assert!(task.restored_partial_pieces.contains(&0));
        assert!(!task.can_aggregate_piece_write(0));

        task.picker.availability.add_have(0);
        let request = task.picker.pick(&PieceBitmap::from_bools(&[true])).unwrap();
        assert_eq!(request.begin, MAX_BLOCK_SIZE);
        task.handle_block(BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: bytes::Bytes::from(second.clone()),
        })
        .await;

        assert!(task.picker.is_complete());
        assert!(task.piece_assemblies.is_empty());
        assert!(!task.restored_partial_pieces.contains(&0));
        let on_disk = std::fs::read(file_path).unwrap();
        assert_eq!(&on_disk[..first.len()], first.as_slice());
        assert_eq!(&on_disk[first.len()..], second.as_slice());
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

        assert_eq!(evictions, vec![1]);
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

        assert_eq!(evictions, vec![1]);
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

        assert!(evictions.is_empty());
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
        assert_eq!(
            memory_aware_request_pipeline(1, 0),
            PEER_REQUEST_PIPELINE_NORMAL
        );
    }

    #[test]
    fn tracker_peer_cache_cap_scales_with_peer_limit() {
        assert_eq!(tracker_peer_cache_cap(1), TRACKER_PEER_CACHE_MIN);
        assert_eq!(tracker_peer_cache_cap(100), 400);
        assert_eq!(tracker_peer_cache_cap(usize::MAX), MAX_TRACKER_PEERS);
    }

    #[test]
    fn tracker_peer_cache_reservation_is_owned_until_cache_drop() {
        let capacity = tracker_peer_cache_cap(1);
        let bytes = capacity as u64 * TRACKER_PEER_CACHE_BYTES_PER_ENTRY;
        let mut caps = [0; rt_metrics::MEMORY_CLASS_COUNT];
        caps[MemoryClass::TrackerPeers as usize] = bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes,
            class_caps_bytes: caps,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let (cache, lease) = prepare_tracker_peer_cache(&governor, 1);
        assert!(cache.capacity() >= capacity);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::TrackerPeers as usize].used_bytes,
            bytes
        );
        drop(cache);
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::TrackerPeers as usize].used_bytes,
            0
        );
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

    #[tokio::test]
    async fn webseed_rate_wait_does_not_block_lifecycle_commands() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let webseed_addr = listener.local_addr().unwrap();
        let (request_started_tx, request_started_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let _ = request_started_tx.send(());
            stream
                .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/4\r\nConnection: close\r\n\r\ndata")
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        });

        let meta = TorrentMetaV1 {
            info_hash: [26; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: vec![format!("http://{webseed_addr}/payload.bin")],
            comment: None,
            created_by: None,
            creation_date: None,
            name: "payload.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("payload.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 4;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let resources = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default());
        let budget = GlobalNetworkBudget::new(8, Some(1), None);
        // Start the actor with an empty global bucket. A four-byte webseed
        // block then waits about four seconds at one byte/sec unless a
        // lifecycle command can interrupt the actor.
        budget.download().acquire(64 * 1024).await;
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
            DbExecutor::direct(Arc::clone(&db)),
            resources,
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
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            budget,
            10_000,
            None,
        )
        .await;
        let task = tokio::spawn(task.run());

        timeout(Duration::from_secs(2), request_started_rx)
            .await
            .expect("webseed request did not start")
            .expect("webseed test server dropped before the request");

        let (pause_reply, pause_result) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), pause_result)
                .await
                .expect("pause should interrupt the global download wait")
                .unwrap(),
            Ok(())
        );

        let (webseed_reply, webseed_result) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::GetWebseeds {
                reply: webseed_reply,
            })
            .await
            .unwrap();
        let snapshots = timeout(Duration::from_secs(1), webseed_result)
            .await
            .expect("webseed snapshot query should complete after pause")
            .unwrap();
        assert_eq!(snapshots.len(), 1);
        assert!(!snapshots[0].is_downloading);
        assert_eq!(snapshots[0].download_rate, 0);

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), task)
            .await
            .expect("webseed rate wait should not strand shutdown")
            .unwrap();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn live_pause_does_not_block_shutdown_on_slow_stopped_announce() {
        use tokio::io::AsyncReadExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker_addr = listener.local_addr().unwrap();
        let (tracker_request_tx, mut tracker_request_rx) = mpsc::unbounded_channel();
        let tracker = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let request_tx = tracker_request_tx.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = stream.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let stopped = request
                        .windows(b"event=stopped".len())
                        .any(|window| window == b"event=stopped");
                    let _ = request_tx.send(stopped);
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });

        let temp = tempfile::tempdir().unwrap();
        let tracker_url = format!("http://{tracker_addr}/announce");
        let meta = TorrentMetaV1 {
            info_hash: [27; 20],
            announce: Some(tracker_url),
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "slow-stop.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("slow-stop.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let info_hash = hex::encode(meta.info_hash);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = 4;
        entry.amount_left = 4;
        entry.transition(TorrentState::Downloading).unwrap();
        persist_task_row(&conn, &entry, &meta);
        let mut fastresume = FastresumeState::new_empty(
            &meta.info_hash,
            meta.pieces.len() as u32,
            ImportPolicy::RequireVerification,
        );
        fastresume.clean_shutdown = true;
        FastresumeStore::new(temp.path().join("fastresume"))
            .save(&fastresume)
            .unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
            DbExecutor::direct(Arc::clone(&db)),
            ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            cmd_rx,
            temp.path().join("fastresume"),
            8,
            6881,
            30,
            1,
            60,
            1024 * 1024,
            StorageIoConfig::default(),
            false,
            OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            },
            GlobalNetworkBudget::unlimited(),
            10_000,
            None,
        )
        .await;
        let actor = tokio::spawn(task.run());

        timeout(Duration::from_secs(2), async {
            loop {
                let state = db
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM torrents WHERE info_hash = ?1",
                        [&info_hash],
                        |row| row.get::<_, String>(0),
                    )
                    .ok();
                if state.as_deref() == Some(TorrentState::Downloading.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("live torrent did not reach its active state");

        let (pause_reply, pause_result) = oneshot::channel();
        cmd_tx
            .send(TorrentCmd::Pause {
                reply: Some(pause_reply),
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), pause_result)
                .await
                .expect("pause reply should be durable before stopped announce")
                .unwrap(),
            Ok(())
        );
        timeout(Duration::from_secs(1), async {
            loop {
                match tracker_request_rx.recv().await {
                    Some(true) => break,
                    Some(false) => {}
                    None => panic!("tracker test server dropped before stopped announce"),
                }
            }
        })
        .await
        .expect("stopped tracker announce did not start");

        cmd_tx.send(TorrentCmd::Shutdown).await.unwrap();
        timeout(Duration::from_secs(1), async {
            loop {
                match tracker_request_rx.recv().await {
                    Some(true) => break,
                    Some(false) => {}
                    None => panic!("tracker test server dropped before shutdown announce"),
                }
            }
        })
        .await
        .expect("shutdown did not retry the interrupted stopped announce");
        timeout(Duration::from_secs(2), actor)
            .await
            .expect("shutdown should interrupt the live stopped tracker announce")
            .unwrap();
        tracker.abort();
        let _ = tracker.await;
    }

    #[test]
    fn tracker_peer_cache_drops_new_peers_after_cap() {
        let peers = [
            SocketAddr::from(([127, 0, 0, 1], 6881)),
            SocketAddr::from(([127, 0, 0, 2], 6881)),
            SocketAddr::from(([127, 0, 0, 3], 6881)),
        ];
        let mut known = HashSet::new();

        let dropped = remember_tracker_peers_bounded(&mut known, &peers, 2);

        assert_eq!(known.len(), 2);
        assert_eq!(dropped, 1);

        let duplicate = *known.iter().next().unwrap();
        let dropped = remember_tracker_peers_bounded(&mut known, &[duplicate], 2);

        assert_eq!(known.len(), 2);
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
    fn tracker_success_does_not_clear_a_newer_lifecycle_event() {
        assert_eq!(
            tracker_event_after_success(TrackerEvent::Completed, TrackerEvent::Started),
            TrackerEvent::Completed
        );
        assert_eq!(
            tracker_event_after_success(TrackerEvent::Started, TrackerEvent::Completed),
            TrackerEvent::Started
        );
    }

    #[test]
    fn incomplete_recheck_discards_stale_terminal_tracker_events() {
        assert_eq!(
            tracker_event_after_incomplete_recheck(TrackerEvent::Started),
            TrackerEvent::Started
        );
        assert_eq!(
            tracker_event_after_incomplete_recheck(TrackerEvent::Empty),
            TrackerEvent::Empty
        );
        assert_eq!(
            tracker_event_after_incomplete_recheck(TrackerEvent::Completed),
            TrackerEvent::Empty
        );
        assert_eq!(
            tracker_event_after_incomplete_recheck(TrackerEvent::Stopped),
            TrackerEvent::Empty
        );
    }

    #[tokio::test]
    async fn restarting_tracker_session_schedules_a_new_started_event_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [25; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "tracker-session.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("tracker-session.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            registry,
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
            None,
        )
        .await;

        let tracker_urls = vec![
            "http://tracker-a/announce".to_owned(),
            "http://tracker-b/announce".to_owned(),
        ];
        task.tracker_tiers = tracker_tiers_from_urls(&tracker_urls);
        for tracker in task.tracker_tiers.iter_mut().flatten() {
            tracker.next_announce = Some(Instant::now() + Duration::from_secs(3600));
        }
        task.tracker_event = TrackerEvent::Completed;
        task.stopped_announced = true;

        task.restart_tracker_session();

        assert_eq!(task.tracker_event, TrackerEvent::Started);
        assert!(!task.stopped_announced);
        assert!(task
            .tracker_tiers
            .iter()
            .flatten()
            .all(|tracker| tracker.is_due()));
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

    #[tokio::test]
    async fn private_tracker_failover_disconnects_old_peers_and_clears_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [26; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "private-failover.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: SafeRelPath::from_name("private-failover.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: true,
            raw: Vec::new(),
        };
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            Arc::new(RwLock::new(SessionRegistry::new())),
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
            None,
        )
        .await;
        task.tracker_tiers = vec![vec![
            TrackerState::new("https://a.example/announce"),
            TrackerState::new("https://b.example/announce"),
        ]];
        task.private_tracker_key = Some((0, 0));

        let peer: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let (_peer_id, _peer_cmd_rx) = task
            .register_peer(peer, permits.acquire_owned().await.unwrap())
            .expect("peer registration should fit the memory budget");
        task.known_tracker_peers.insert(peer);

        task.advance_private_tracker_after_failure((0, 0));

        assert_eq!(task.private_tracker_key, Some((0, 1)));
        assert!(task.active_peers.is_empty());
        assert!(task.known_tracker_peers.is_empty());
        assert!(task.tracker_tiers[0][1].is_due());
    }

    #[test]
    fn v1_padding_bytes_must_be_zero() {
        let files = vec![
            TorrentFileV1 {
                index: 0,
                length: 3,
                path: SafeRelPath::from_name("payload.bin", false).unwrap(),
                offset: 0,
                pad: false,
            },
            TorrentFileV1 {
                index: 1,
                length: 13,
                path: SafeRelPath::from_components(&[".pad", "13"], false).unwrap(),
                offset: 3,
                pad: true,
            },
        ];
        let piece_map = build_piece_map(16, &files).unwrap();
        let mut block = b"abc".to_vec();
        block.resize(16, 0);
        assert!(validate_padding_block(&piece_map, 0, 0, &block).is_ok());

        block[7] = 1;
        assert!(validate_padding_block(&piece_map, 0, 0, &block).is_err());
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

    #[test]
    fn removing_peer_availability_accepts_the_packed_bitmap_directly() {
        let mut availability = Availability::new(10);
        for piece in [0, 2, 7, 9] {
            availability.add_have(piece);
        }
        let peer_has = PieceBitmap::from_bools(&[
            true, false, true, false, false, false, false, true, false, true,
        ]);

        remove_peer_availability(&mut availability, &peer_has);

        for piece in 0..10 {
            assert_eq!(availability.count(piece), 0, "piece {piece}");
        }
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
            _bitmap_memory_lease: None,
            metadata: None,
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: None,
            upload_control: RateLimitCancellation::default(),
            shutdown_control: RateLimitCancellation::default(),
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
        };

        let read_context = UploadReadContext {
            save_root: upload.save_root.clone(),
            piece_map: upload.piece_map.clone(),
            storage: upload.storage.clone(),
            resources: upload.resources.clone(),
        };
        let block = read_upload_block(&read_context, 0, 0, 16 * 1024)
            .await
            .unwrap();

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

    #[tokio::test]
    async fn upload_block_synthesizes_missing_bep47_padding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("payload.bin"), b"abc").unwrap();
        let files = vec![
            FileSpan {
                file_index: 0,
                path: SafeRelPath::from_name("payload.bin", false).unwrap(),
                content_offset: 0,
                length: 3,
            },
            FileSpan {
                file_index: 1,
                path: SafeRelPath::from_components(&[".pad", "13"], false).unwrap(),
                content_offset: 3,
                length: 13,
            },
        ];
        let piece_map = Arc::new(PieceMap::new_with_padding(16, files, [1]).unwrap());
        let upload = UploadReadContext {
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
        };

        let block = read_upload_block(&upload, 0, 0, 16).await.unwrap();
        let mut expected = b"abc".to_vec();
        expected.resize(16, 0);
        assert_eq!(block.data.as_ref(), expected.as_slice());
        assert!(!dir.path().join(".pad/13").exists());
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
    fn metadata_upload_payload_is_shared_and_budgeted_per_torrent() {
        let pieces = [0_u8; 20];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(4)),
            (b"name", BValue::Bytes(b"a.bin")),
            (b"piece length", BValue::Int(16_384)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let raw = rt_bencode::encode(&BValue::Dict(vec![(
            b"info".as_slice(),
            BValue::Dict(info_pairs),
        )]));
        let payload_len = torrent_info_bytes(&raw).unwrap().len();
        let mut class_caps_bytes = [0; rt_metrics::MEMORY_CLASS_COUNT];
        let class_cap_bytes = payload_len as u64 + 1024 * 1024;
        class_caps_bytes[MemoryClass::Metadata as usize] = class_cap_bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: class_cap_bytes,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let (metadata, lease) = prepare_metadata_payload(&governor, "a".repeat(40).as_str(), &raw);
        let metadata = metadata.expect("metadata payload should fit the class budget");
        let peer_a = metadata.clone();
        let peer_b = metadata.clone();
        assert!(Arc::ptr_eq(&metadata, &peer_a));
        assert!(Arc::ptr_eq(&peer_a, &peer_b));
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            payload_len as u64
        );
        drop(peer_a);
        drop(peer_b);
        drop(metadata);
        drop(lease);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            0
        );
    }

    #[test]
    fn tracker_state_reservation_covers_bounded_response_fields() {
        let mut tiers = tracker_tiers_from_urls(&["https://tracker.example/announce".to_owned()]);
        let bytes = tracker_state_memory_bytes(&tiers, tiers.capacity()) as u64;
        let mut class_caps_bytes = [0; rt_metrics::MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::Metadata as usize] = bytes;
        let governor = ResourceGovernor::new(rt_metrics::ResourceGovernorConfig {
            total_cap_bytes: bytes,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let leases = reserve_tracker_state_memory(&governor, &tiers, tiers.capacity()).unwrap();
        assert_eq!(leases.iter().map(MemoryLease::bytes).sum::<u64>(), bytes);

        let tracker = tiers[0].first_mut().unwrap();
        tracker.on_success(&AnnounceResponse {
            interval: 1_800,
            min_interval: None,
            peers: Vec::new(),
            tracker_id: Some(vec![0xabu8; MAX_TRACKER_STATE_ID_BYTES]),
            warning_message: Some("warning".repeat(MAX_TRACKER_STATE_TEXT_BYTES / 7)),
            complete: None,
            incomplete: None,
        });
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
            bytes
        );

        drop(leases);
        assert_eq!(
            governor.snapshot().classes[MemoryClass::Metadata as usize].used_bytes,
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

    #[tokio::test]
    async fn peer_event_delivery_is_bounded_when_torrent_actor_stalls() {
        let (tx, mut rx) = mpsc::channel(1);
        let peer_id = PeerId::new();
        tx.send(PeerEvent::Unchoked {
            peer: "127.0.0.1:6881".parse().unwrap(),
            id: peer_id,
        })
        .await
        .unwrap();

        let started = Instant::now();
        let delivered = send_peer_event(
            &tx,
            PeerEvent::Interested {
                peer: "127.0.0.1:6881".parse().unwrap(),
                id: peer_id,
            },
        )
        .await;

        assert!(!delivered);
        assert!(started.elapsed() < PEER_EVENT_SEND_TIMEOUT + Duration::from_secs(1));
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn upload_reads_are_aborted_after_peer_loop_exit() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DropMarker(Arc<AtomicBool>);

        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let task_started = Arc::clone(&started);
        let read = tokio::spawn(async move {
            task_started.store(true, Ordering::SeqCst);
            let _marker = DropMarker(marker);
            std::future::pending::<UploadReadResult>().await
        });
        timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("upload read task did not start");

        let upload_reads = futures::stream::FuturesUnordered::new();
        upload_reads.push(read);

        abort_upload_reads(&upload_reads);

        timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborting an upload read must cancel its task");
    }

    #[tokio::test]
    async fn upload_reads_are_aborted_when_peer_loop_is_cancelled() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DropMarker(Arc<AtomicBool>);

        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let owner_started = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let task_started = Arc::clone(&started);
        let read = tokio::spawn(async move {
            task_started.store(true, Ordering::SeqCst);
            let _marker = DropMarker(marker);
            std::future::pending::<UploadReadResult>().await
        });
        timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("upload read task did not start");

        let task_owner_started = Arc::clone(&owner_started);
        let peer_loop = tokio::spawn(async move {
            let upload_reads = UploadReadTasks::default();
            upload_reads.push(read);
            task_owner_started.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        timeout(Duration::from_secs(1), async {
            while !owner_started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("upload read owner did not start");
        peer_loop.abort();
        assert!(peer_loop.await.unwrap_err().is_cancelled());

        timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelling the peer loop must cancel its upload read tasks");
    }

    #[tokio::test]
    async fn failed_choke_event_preserves_outstanding_requests() {
        let (tx, _rx) = mpsc::channel(1);
        let peer = "127.0.0.1:6881".parse().unwrap();
        let peer_id = PeerId::new();
        tx.try_send(PeerEvent::Unchoked { peer, id: peer_id })
            .unwrap();
        let request = BlockRequest {
            piece: 3,
            begin: 0,
            length: 4,
        };
        let mut outstanding = Vec::new();

        assert!(!send_choke_event(&tx, peer, peer_id, vec![request], &mut outstanding,).await);
        assert_eq!(drain_outstanding(&mut outstanding), vec![request]);
    }

    #[tokio::test]
    async fn terminal_peer_disconnect_waits_for_dedicated_queue_capacity() {
        let (tx, mut rx) = mpsc::channel(1);
        let peer = "127.0.0.1:6881".parse().unwrap();
        let peer_id = PeerId::new();
        tx.send(PeerEvent::Unchoked { peer, id: peer_id })
            .await
            .unwrap();

        let sender = tokio::spawn(async move {
            send_peer_disconnect_event(
                &tx,
                PeerEvent::Disconnected {
                    peer,
                    id: peer_id,
                    outstanding: Vec::new(),
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(
            !sender.is_finished(),
            "terminal disconnect must not be dropped when its queue is full"
        );

        assert!(matches!(rx.recv().await, Some(PeerEvent::Unchoked { .. })));
        assert!(sender.await.unwrap());
        assert!(matches!(
            rx.recv().await,
            Some(PeerEvent::Disconnected { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn upload_budget_handles_a_block_larger_than_the_rate_limit() {
        let waiter = tokio::spawn(async {
            let mut tokens = 1_000;
            let mut tokens_updated = TokioInstant::now();
            let control = RateLimitCancellation::default();
            let generation = control.generation();
            wait_for_upload_budget(
                Some(1_000),
                &mut tokens,
                &mut tokens_updated,
                2_000,
                &control,
                generation,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        let finished = waiter.is_finished();
        if !finished {
            waiter.abort();
        }
        assert!(
            finished,
            "an upload block larger than the rate bucket must complete in chunks"
        );
        assert!(waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn upload_budget_wakes_when_peer_control_changes() {
        let control = RateLimitCancellation::default();
        let generation = control.generation();
        let waiter_control = control.clone();
        let waiter = tokio::spawn(async move {
            let mut tokens = 0;
            let mut tokens_updated = TokioInstant::now();
            wait_for_upload_budget(
                Some(1),
                &mut tokens,
                &mut tokens_updated,
                1,
                &waiter_control,
                generation,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        control.cancel();
        assert!(!waiter.await.unwrap());
    }

    #[tokio::test]
    async fn peer_loop_shutdown_interrupts_rate_limited_upload() {
        let temp = tempfile::tempdir().unwrap();
        let payload = vec![7u8; MAX_BLOCK_SIZE as usize];
        std::fs::write(temp.path().join("peer.bin"), &payload).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let remote = TcpStream::connect(peer_addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        let piece_map = Arc::new(
            PieceMap::new(
                u64::from(MAX_BLOCK_SIZE),
                vec![FileSpan {
                    file_index: 0,
                    path: rt_path::SafeRelPath::from_name("peer.bin", false).unwrap(),
                    content_offset: 0,
                    length: u64::from(MAX_BLOCK_SIZE),
                }],
            )
            .unwrap(),
        );
        let control = RateLimitCancellation::default();
        let shutdown_control = RateLimitCancellation::default();
        let upload = UploadContext {
            save_root: temp.path().to_path_buf(),
            piece_map,
            storage: MountScheduler::new_for_path(
                StorageRootId::new(),
                temp.path(),
                &SchedulerConfig {
                    profile: StorageProfile::Unknown,
                    ..Default::default()
                },
            ),
            resources: ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            have_pieces: PieceBitmap::from_bools(&[true]),
            _bitmap_memory_lease: None,
            metadata: None,
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: Some(1),
            upload_control: control.clone(),
            shutdown_control: shutdown_control.clone(),
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
        };
        let (peer_event_tx, mut peer_event_rx) = mpsc::channel(4);
        let (peer_cmd_tx, peer_cmd_rx) = mpsc::channel(4);
        let loop_task = tokio::spawn(run_peer_loop(
            peer_addr,
            PeerId::new(),
            PeerIo::Tcp(Framed::new(server, PeerCodec::default())),
            peer_event_tx,
            peer_cmd_rx,
            upload,
            true,
        ));
        let mut remote = Framed::new(remote, PeerCodec::default());

        peer_cmd_tx.send(PeerCommand::Unchoke).await.unwrap();
        assert!(matches!(
            remote.next().await.unwrap().unwrap(),
            Message::Unchoke
        ));
        remote
            .send(Message::Request {
                piece: 0,
                begin: 0,
                length: MAX_BLOCK_SIZE,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        // The one-byte-per-second bucket would otherwise keep this peer loop
        // in its upload wait for hours after the first byte is consumed.
        control.cancel();
        shutdown_control.cancel();
        peer_cmd_tx.send(PeerCommand::Shutdown).await.unwrap();
        let exit = timeout(Duration::from_secs(1), loop_task)
            .await
            .expect("shutdown must interrupt a rate-limited upload wait")
            .unwrap();
        assert!(exit.result.is_ok());
        assert!(matches!(
            peer_event_rx.recv().await,
            Some(PeerEvent::Disconnected { peer, .. }) if peer == peer_addr
        ));
    }

    #[tokio::test]
    async fn peer_loop_exits_when_command_channel_closes() {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(peer_addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        let piece_map = Arc::new(
            PieceMap::new(
                4,
                vec![FileSpan {
                    file_index: 0,
                    path: rt_path::SafeRelPath::from_name("peer.bin", false).unwrap(),
                    content_offset: 0,
                    length: 4,
                }],
            )
            .unwrap(),
        );
        let upload = UploadContext {
            save_root: temp.path().to_path_buf(),
            piece_map,
            storage: MountScheduler::new_for_path(
                StorageRootId::new(),
                temp.path(),
                &SchedulerConfig {
                    profile: StorageProfile::Unknown,
                    ..Default::default()
                },
            ),
            resources: ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            have_pieces: PieceBitmap::from_bools(&[false]),
            _bitmap_memory_lease: None,
            metadata: None,
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: None,
            upload_control: RateLimitCancellation::default(),
            shutdown_control: RateLimitCancellation::default(),
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
        };
        let (peer_event_tx, mut peer_event_rx) = mpsc::channel(2);
        let (peer_cmd_tx, peer_cmd_rx) = mpsc::channel(1);
        drop(peer_cmd_tx);

        let result = timeout(
            Duration::from_secs(1),
            run_peer_loop(
                peer_addr,
                PeerId::new(),
                PeerIo::Tcp(Framed::new(server, PeerCodec::default())),
                peer_event_tx,
                peer_cmd_rx,
                upload,
                true,
            ),
        )
        .await
        .expect("closed command channel should stop the peer promptly");

        assert!(result.result.is_ok());
        assert!(matches!(
            peer_event_rx.recv().await,
            Some(PeerEvent::Disconnected { peer, .. }) if peer == peer_addr
        ));
        drop(client);
    }

    #[tokio::test]
    async fn peer_loop_uses_remote_metadata_extension_id_for_upload() {
        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let remote = TcpStream::connect(peer_addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        let piece_map = Arc::new(
            PieceMap::new(
                4,
                vec![FileSpan {
                    file_index: 0,
                    path: rt_path::SafeRelPath::from_name("peer.bin", false).unwrap(),
                    content_offset: 0,
                    length: 4,
                }],
            )
            .unwrap(),
        );
        let upload = UploadContext {
            save_root: temp.path().to_path_buf(),
            piece_map,
            storage: MountScheduler::new_for_path(
                StorageRootId::new(),
                temp.path(),
                &SchedulerConfig {
                    profile: StorageProfile::Unknown,
                    ..Default::default()
                },
            ),
            resources: ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            have_pieces: PieceBitmap::from_bools(&[false]),
            _bitmap_memory_lease: None,
            metadata: Some(Arc::new(b"metadata".to_vec())),
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: None,
            upload_control: RateLimitCancellation::default(),
            shutdown_control: RateLimitCancellation::default(),
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
        };
        let (peer_event_tx, mut peer_event_rx) = mpsc::channel(4);
        let (peer_cmd_tx, peer_cmd_rx) = mpsc::channel(1);
        let loop_task = tokio::spawn(run_peer_loop(
            peer_addr,
            PeerId::new(),
            PeerIo::Tcp(Framed::new(server, PeerCodec::default())),
            peer_event_tx,
            peer_cmd_rx,
            upload,
            true,
        ));
        let mut remote = Framed::new(remote, PeerCodec::default());

        remote
            .send(Message::Extended {
                ext_id: EXT_HANDSHAKE_ID,
                payload: ExtensionHandshake::new(None).with_ut_metadata(7).encode(),
            })
            .await
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(1), peer_event_rx.recv())
                .await
                .unwrap(),
            Some(PeerEvent::ExtendedHandshake {
                ut_metadata_id: Some(7),
                ..
            })
        ));

        remote
            .send(Message::Extended {
                // This is TorrentNG's id, which the remote learned from our
                // handshake. The response must use the remote's id (7).
                ext_id: LOCAL_UT_METADATA_ID,
                payload: UtMetadataMessage::Request { piece: 0 }.encode(),
            })
            .await
            .unwrap();

        let response = timeout(Duration::from_secs(1), remote.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Extended { ext_id, payload } = response else {
            panic!("expected a ut_metadata response");
        };
        assert_eq!(ext_id, 7);
        assert_eq!(
            UtMetadataMessage::parse(&payload).unwrap(),
            UtMetadataMessage::Data {
                piece: 0,
                total_size: 8,
                data: b"metadata".to_vec(),
            }
        );

        drop(peer_cmd_tx);
        let exit = timeout(Duration::from_secs(1), loop_task)
            .await
            .expect("peer loop should stop when its command channel closes")
            .unwrap();
        assert!(exit.result.is_ok());
    }

    #[tokio::test]
    async fn peer_loop_returns_outstanding_requests_on_disconnect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let temp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = listener.local_addr().unwrap();
        let remote = TcpStream::connect(peer_addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();

        let remote_task = tokio::spawn(async move {
            let mut remote = remote;
            remote.write_all(&Message::Unchoke.encode()).await.unwrap();
            let mut request = [0u8; 128];
            let _ = remote.read(&mut request).await;
        });

        let piece_map = Arc::new(
            PieceMap::new(
                4,
                vec![FileSpan {
                    file_index: 0,
                    path: rt_path::SafeRelPath::from_name("peer.bin", false).unwrap(),
                    content_offset: 0,
                    length: 4,
                }],
            )
            .unwrap(),
        );
        let upload = UploadContext {
            save_root: temp.path().to_path_buf(),
            piece_map,
            storage: MountScheduler::new_for_path(
                StorageRootId::new(),
                temp.path(),
                &SchedulerConfig {
                    profile: StorageProfile::Unknown,
                    ..Default::default()
                },
            ),
            resources: ResourceGovernor::new(rt_metrics::ResourceGovernorConfig::default()),
            have_pieces: PieceBitmap::from_bools(&[false]),
            _bitmap_memory_lease: None,
            metadata: None,
            is_private: false,
            pex_enabled: true,
            upload_limit_bytes_per_sec: None,
            upload_control: RateLimitCancellation::default(),
            shutdown_control: RateLimitCancellation::default(),
            global_download: GlobalNetworkBudget::unlimited().download(),
            global_upload: GlobalNetworkBudget::unlimited().upload(),
        };
        let (peer_event_tx, mut peer_event_rx) = mpsc::channel(4);
        let (peer_cmd_tx, peer_cmd_rx) = mpsc::channel(4);
        let loop_task = tokio::spawn(run_peer_loop(
            peer_addr,
            PeerId::new(),
            PeerIo::Tcp(Framed::new(server, PeerCodec::default())),
            peer_event_tx,
            peer_cmd_rx,
            upload,
            false,
        ));

        assert!(matches!(
            peer_event_rx.recv().await,
            Some(PeerEvent::Unchoked { peer, .. }) if peer == peer_addr
        ));
        let request = BlockRequest {
            piece: 0,
            begin: 0,
            length: 4,
        };
        peer_cmd_tx
            .send(PeerCommand::Request(request))
            .await
            .unwrap();

        let exit = timeout(Duration::from_secs(2), loop_task)
            .await
            .expect("peer loop did not exit after remote disconnect")
            .unwrap();
        assert!(exit.outstanding.contains(&request));
        remote_task.await.unwrap();
    }

    #[test]
    fn peer_event_channel_capacity_is_bounded_by_global_peer_budget() {
        assert_eq!(peer_event_channel_capacity(1), 64);
        assert_eq!(peer_event_channel_capacity(200), 200);
        assert_eq!(peer_event_channel_capacity(10_000), 512);
    }

    #[test]
    fn paused_torrent_drops_late_peer_events() {
        let peer_id = PeerId::new();
        let active_event = PeerEvent::Unchoked {
            peer: "127.0.0.1:6881".parse().unwrap(),
            id: peer_id,
        };
        let late_piece = PeerEvent::Piece {
            peer: "127.0.0.1:6881".parse().unwrap(),
            id: peer_id,
            block: BlockEvent {
                piece: 0,
                offset: 0,
                data: bytes::Bytes::from_static(b"late"),
            },
            _memory_lease: None,
        };

        assert!(peer_event_if_active(false, active_event).is_some());
        assert!(peer_event_if_active(true, late_piece).is_none());
    }

    #[tokio::test]
    async fn last_peer_disconnect_preserves_partial_assembly() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let piece_length = u64::from(MAX_BLOCK_SIZE) * 2;
        let meta = TorrentMetaV1 {
            info_hash: [25; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "disconnect-partial.bin".into(),
            piece_length,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: piece_length,
                path: rt_path::SafeRelPath::from_name("disconnect-partial.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            Arc::new(RwLock::new(SessionRegistry::new())),
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
            None,
        )
        .await;

        let peer_addr = "127.0.0.1:6881".parse().unwrap();
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let (peer_id, _peer_cmd_rx) = task
            .register_peer(peer_addr, permits.acquire_owned().await.unwrap())
            .expect("peer registration should fit the memory budget");
        task.picker.availability.add_have(0);
        let request = task.picker.pick(&PieceBitmap::from_bools(&[true])).unwrap();
        task.record_piece_block(&BlockEvent {
            piece: request.piece,
            offset: request.begin,
            data: bytes::Bytes::from(vec![0xA5; request.length as usize]),
        })
        .unwrap();
        assert!(!task
            .picker
            .block_received(request.piece as usize, request.begin));

        task.handle_peer_event(PeerEvent::Disconnected {
            peer: peer_addr,
            id: peer_id,
            outstanding: vec![request],
        })
        .await;

        assert!(task.active_peers.is_empty());
        assert!(task.piece_assemblies.contains_key(&request.piece));
        assert_eq!(task.picker.partial_pieces(), vec![(request.piece, vec![0])]);
    }

    #[tokio::test]
    async fn stale_peer_events_cannot_mutate_a_reconnected_peer() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [18; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "peer-reconnect.bin".into(),
            piece_length: 4,
            pieces: vec![[0; 20]],
            files: vec![rt_metainfo::TorrentFileV1 {
                index: 0,
                length: 4,
                path: rt_path::SafeRelPath::from_name("peer-reconnect.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: Vec::new(),
        };
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            Arc::new(RwLock::new(SessionRegistry::new())),
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
            None,
        )
        .await;

        let peer_addr = "127.0.0.1:6881".parse().unwrap();
        let permits = Arc::new(tokio::sync::Semaphore::new(2));
        let (old_id, old_cmd_rx) = task
            .register_peer(
                peer_addr,
                Arc::clone(&permits).acquire_owned().await.unwrap(),
            )
            .expect("old peer registration should fit the memory budget");
        task.active_peers.remove(&peer_addr);
        drop(old_cmd_rx);
        let (new_id, _new_cmd_rx) = task
            .register_peer(peer_addr, permits.acquire_owned().await.unwrap())
            .expect("new peer registration should fit the memory budget");
        assert_ne!(old_id, new_id);

        task.handle_peer_event(PeerEvent::Interested {
            peer: peer_addr,
            id: old_id,
        })
        .await;
        task.handle_peer_event(PeerEvent::Disconnected {
            peer: peer_addr,
            id: old_id,
            outstanding: Vec::new(),
        })
        .await;

        let current = task
            .active_peers
            .get(&peer_addr)
            .expect("stale disconnect removed the reconnected peer");
        assert_eq!(current.id, new_id);
        assert!(!current.interested);

        task.handle_peer_event(PeerEvent::Interested {
            peer: peer_addr,
            id: new_id,
        })
        .await;
        assert!(task.active_peers.get(&peer_addr).unwrap().interested);
    }

    #[tokio::test]
    async fn shutdown_peers_clears_picker_peer_state() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let meta = TorrentMetaV1 {
            info_hash: [9; 20],
            announce: None,
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 4,
            pieces: vec![[8; 20]],
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
        let db = Arc::new(Mutex::new(conn));
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let mut task = TorrentTask::new(
            meta,
            temp.path().to_path_buf(),
            false,
            TorrentState::Downloading,
            Arc::new(RwLock::new(SessionRegistry::new())),
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
            None,
        )
        .await;

        let peer_addr = "127.0.0.1:6881".parse().unwrap();
        let (peer_cmd_tx, _peer_cmd_rx) = mpsc::channel(1);
        let peer_permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        task.picker.availability.add_bitfield(&[0x80]);
        task.picker.pick_from_seed().unwrap();
        let peer_has = PieceBitmap::from_bools(&[true]);
        let pending_have = PieceBitmap::new(1);
        let bitmap_memory_lease = task
            .resources
            .try_acquire(
                MemoryClass::PeerBuffer,
                peer_has
                    .memory_bytes()
                    .saturating_add(pending_have.memory_bytes()),
            )
            .expect("test peer state should fit the memory budget");
        task.active_peers.insert(
            peer_addr,
            PeerHandle {
                id: PeerId::new(),
                cmd_tx: peer_cmd_tx,
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

        assert_eq!(task.picker.availability.count(0), 1);
        task.shutdown_peers().await;

        assert!(task.active_peers.is_empty());
        assert_eq!(task.picker.availability.count(0), 0);
        assert!(task.picker.pick_from_seed().is_some());
    }
}
