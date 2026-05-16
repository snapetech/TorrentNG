/// Commands sent from the API layer down to the engine or individual torrent tasks.
use std::path::PathBuf;

use tokio::sync::oneshot;

use rt_metainfo::{MagnetLink, TorrentMeta};

pub type CmdResult<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTorrentFile {
    pub index: u32,
    pub path: String,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTorrentMetadata {
    pub piece_length: u64,
    pub piece_count: usize,
    pub is_private: bool,
    pub trackers: Vec<String>,
    pub files: Vec<EngineTorrentFile>,
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
    /// Graceful shutdown.
    Shutdown,
}
