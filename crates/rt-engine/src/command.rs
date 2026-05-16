/// Commands sent from the API layer down to the engine or individual torrent tasks.
use std::{net::SocketAddr, path::PathBuf};

use tokio::sync::oneshot;

use rt_metainfo::{MagnetLink, TorrentMeta};

pub type CmdResult<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTorrentFile {
    pub index: u32,
    pub path: String,
    pub length: u64,
    pub priority: i64,
    pub wanted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePieceState {
    Missing,
    Partial,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTorrentMetadata {
    pub piece_length: u64,
    pub piece_count: usize,
    pub piece_hashes: Vec<String>,
    pub piece_states: Vec<EnginePieceState>,
    pub is_private: bool,
    pub trackers: Vec<String>,
    pub webseeds: Vec<String>,
    pub files: Vec<EngineTorrentFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub torrents_total: u64,
    pub torrents_seeding: u64,
    pub torrents_downloading: u64,
    pub torrents_paused: u64,
    pub torrents_checking: u64,
    pub torrents_queued: u64,
    pub torrents_error: u64,
    pub torrents_metadata_pending: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub bytes_left: u64,
    pub jobs_active: u64,
    pub trackers_total: u64,
    pub trackers_working: u64,
    pub trackers_warning: u64,
    pub trackers_error: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineTorrentLimits {
    pub download_limit: Option<i64>,
    pub upload_limit: Option<i64>,
    pub max_connections: Option<i64>,
    pub seed_ratio_limit: Option<f64>,
    pub seed_idle_limit: Option<i64>,
    pub sequential_download: bool,
    pub first_last_piece_prio: bool,
    pub force_start: bool,
    pub super_seeding: bool,
    pub auto_tmm: bool,
    pub auto_management: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineGlobalLimits {
    pub download_limit: i64,
    pub upload_limit: i64,
    pub speed_limits_mode: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnginePeerSnapshot {
    pub addr: SocketAddr,
    pub client: String,
    pub choked: bool,
    pub upload_choked: bool,
    pub interested: bool,
    pub pieces: usize,
    pub pieces_total: usize,
    pub progress: f64,
    pub download_rate: i64,
    pub upload_rate: i64,
    pub downloaded: u64,
    pub uploaded: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMove {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TorrentDiagnostic {
    pub info_hash: String,
    pub state: String,
    pub is_private: bool,
    pub bytes_left: u64,
    pub active_jobs: usize,
    pub tracker_errors: usize,
    pub tracker_warnings: usize,
    pub reasons: Vec<String>,
    pub next_actions: Vec<String>,
}

/// Engine-level commands (handled by the top-level EngineHandle).
#[derive(Debug)]
pub enum EngineCmd {
    /// Add a torrent from parsed metainfo. save_path overrides config default.
    AddTorrent {
        meta: Box<TorrentMeta>,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>, // returns info_hash hex
    },
    /// Add a magnet as a metadata-pending entry.
    AddMagnet {
        magnet: MagnetLink,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>,
    },
    /// Internal completion from the magnet metadata worker.
    CompleteMagnet { info_hash: String, raw: Vec<u8> },
    /// Remove a torrent. delete_files removes content from disk.
    RemoveTorrent {
        info_hash: String,
        delete_files: bool,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Pause a running torrent.
    PauseTorrent {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Resume a paused torrent.
    ResumeTorrent {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Force recheck piece hashes.
    RecheckTorrent {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Pause a durable job by ID.
    PauseJob {
        job_id: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Resume a durable job by ID.
    ResumeJob {
        job_id: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Cancel a durable job by ID.
    CancelJob {
        job_id: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Force a tracker announce now.
    ReannounceTorrent {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Read persisted metainfo for API detail/file/tracker views.
    GetTorrentMetadata {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<EngineTorrentMetadata>>,
    },
    /// Update category and/or tags, then persist the session row.
    UpdateTorrentLabels {
        info_hash: String,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Update user-visible torrent fields stored in the session row.
    UpdateTorrentFields {
        info_hash: String,
        name: Option<String>,
        save_path: Option<PathBuf>,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Replace persisted tracker URLs for a torrent.
    UpdateTorrentTrackers {
        info_hash: String,
        trackers: Vec<String>,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    GetTorrentLimits {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<EngineTorrentLimits>>,
    },
    UpdateTorrentLimits {
        info_hash: String,
        limits: EngineTorrentLimits,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    UpdateFilePriorities {
        info_hash: String,
        file_ids: Vec<u32>,
        priority: i64,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    RenameFilePath {
        info_hash: String,
        file_id: u32,
        new_path: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    RenameFolderPath {
        info_hash: String,
        old_path: String,
        new_path: String,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    AddPeers {
        info_hash: String,
        peers: Vec<SocketAddr>,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    GetTorrentPeers {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<Vec<EnginePeerSnapshot>>>,
    },
    GetGlobalLimits {
        reply: oneshot::Sender<CmdResult<EngineGlobalLimits>>,
    },
    UpdateGlobalLimits {
        limits: EngineGlobalLimits,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    GetQueuePriority {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<i32>>,
    },
    UpdateQueueOrder {
        info_hashes: Vec<String>,
        queue_move: QueueMove,
        reply: oneshot::Sender<CmdResult<()>>,
    },
    /// Snapshot runtime and durable metrics.
    GetStats {
        reply: oneshot::Sender<CmdResult<EngineStats>>,
    },
    /// Structured diagnostic for why a torrent is not seeding.
    DiagnoseTorrent {
        info_hash: String,
        reply: oneshot::Sender<CmdResult<TorrentDiagnostic>>,
    },
    /// Graceful shutdown.
    Shutdown,
}
