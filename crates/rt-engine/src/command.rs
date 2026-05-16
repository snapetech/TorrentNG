/// Commands sent from the API layer down to the engine or individual torrent tasks.
use std::path::PathBuf;

use tokio::sync::oneshot;

use rt_metainfo::TorrentMeta;

pub type CmdResult<T> = Result<T, String>;

/// Engine-level commands (handled by the top-level EngineHandle).
#[derive(Debug)]
pub enum EngineCmd {
    /// Add a torrent from parsed metainfo. save_path overrides config default.
    AddTorrent {
        meta: Box<TorrentMeta>,
        save_path: Option<PathBuf>,
        paused: bool,
        reply: oneshot::Sender<CmdResult<String>>, // returns info_hash hex
    },
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
    /// Graceful shutdown.
    Shutdown,
}
