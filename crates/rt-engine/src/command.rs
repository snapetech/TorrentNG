/// Commands sent from the API layer down to the engine or individual torrent tasks.
use std::{net::SocketAddr, path::PathBuf};

use tokio::sync::oneshot;

use rt_metainfo::{MagnetLink, TorrentMeta};
use rt_storage::{StorageIoStats, STORAGE_LATENCY_BUCKET_COUNT};

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
    pub storage_file_pool_capacity: u64,
    pub storage_file_pool_open_files: u64,
    pub storage_file_pool_hits: u64,
    pub storage_file_pool_misses: u64,
    pub storage_file_pool_evictions: u64,
    pub storage_file_pool_idle_closes: u64,
    pub storage_io_queue_depth: u64,
    pub storage_hash_queue_depth: u64,
    pub storage_dirty_files: u64,
    pub storage_read_ops: u64,
    pub storage_write_ops: u64,
    pub storage_bytes_read: u64,
    pub storage_bytes_written: u64,
    pub storage_read_ops_by_class: [u64; 6],
    pub storage_write_ops_by_class: [u64; 6],
    pub storage_bytes_read_by_class: [u64; 6],
    pub storage_bytes_written_by_class: [u64; 6],
    pub storage_backend_read_ops: u64,
    pub storage_backend_bytes_read: u64,
    pub storage_backend_read_ops_by_class: [u64; 6],
    pub storage_backend_bytes_read_by_class: [u64; 6],
    pub storage_read_latency_ns: u64,
    pub storage_write_latency_ns: u64,
    pub storage_read_latency_ns_by_class: [u64; 6],
    pub storage_write_latency_ns_by_class: [u64; 6],
    pub storage_read_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub storage_write_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub storage_sync_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub storage_hash_latency_buckets: [u64; STORAGE_LATENCY_BUCKET_COUNT],
    pub storage_sync_latency_ns: u64,
    pub storage_hash_latency_ns: u64,
    pub storage_sync_ops: u64,
    pub storage_hash_ops: u64,
    pub storage_preallocation_failures: u64,
    pub storage_preallocation_fallbacks: u64,
    pub storage_peer_read_cache_entries: u64,
    pub storage_peer_read_cache_hits: u64,
    pub storage_peer_read_cache_misses: u64,
    pub storage_peer_read_elevator_enabled: u64,
    pub storage_peer_read_elevator_queue_depth: u64,
    pub storage_peer_read_elevator_queued: u64,
    pub storage_peer_read_elevator_batches: u64,
    pub storage_peer_read_elevator_coalesced_requests: u64,
    pub piece_assembly_buffers: u64,
    pub piece_assembly_bytes: u64,
    pub piece_assembly_evictions: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TorrentRuntimeStats {
    pub piece_assembly_buffers: u64,
    pub piece_assembly_bytes: u64,
    pub piece_assembly_evictions: u64,
    pub storage: StorageIoStats,
}

impl EngineStats {
    pub fn add_torrent_runtime(&mut self, runtime: TorrentRuntimeStats) {
        self.piece_assembly_buffers = self
            .piece_assembly_buffers
            .saturating_add(runtime.piece_assembly_buffers);
        self.piece_assembly_bytes = self
            .piece_assembly_bytes
            .saturating_add(runtime.piece_assembly_bytes);
        self.piece_assembly_evictions = self
            .piece_assembly_evictions
            .saturating_add(runtime.piece_assembly_evictions);

        let storage = runtime.storage;
        self.storage_file_pool_capacity = self
            .storage_file_pool_capacity
            .saturating_add(storage.file_pool.capacity as u64);
        self.storage_file_pool_open_files = self
            .storage_file_pool_open_files
            .saturating_add(storage.file_pool.open_files as u64);
        self.storage_file_pool_hits = self
            .storage_file_pool_hits
            .saturating_add(storage.file_pool.hits);
        self.storage_file_pool_misses = self
            .storage_file_pool_misses
            .saturating_add(storage.file_pool.misses);
        self.storage_file_pool_evictions = self
            .storage_file_pool_evictions
            .saturating_add(storage.file_pool.evictions);
        self.storage_file_pool_idle_closes = self
            .storage_file_pool_idle_closes
            .saturating_add(storage.file_pool.idle_closes);
        self.storage_io_queue_depth = self
            .storage_io_queue_depth
            .saturating_add(storage.io_queue_depth as u64);
        self.storage_hash_queue_depth = self
            .storage_hash_queue_depth
            .saturating_add(storage.hash_queue_depth as u64);
        self.storage_dirty_files = self
            .storage_dirty_files
            .saturating_add(storage.dirty_files as u64);
        self.storage_read_ops = self
            .storage_read_ops
            .saturating_add(storage.read_ops_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_read_ops_by_class
            .iter_mut()
            .zip(storage.read_ops_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_write_ops = self
            .storage_write_ops
            .saturating_add(storage.write_ops_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_write_ops_by_class
            .iter_mut()
            .zip(storage.write_ops_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_bytes_read = self
            .storage_bytes_read
            .saturating_add(storage.bytes_read_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_bytes_read_by_class
            .iter_mut()
            .zip(storage.bytes_read_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_bytes_written = self
            .storage_bytes_written
            .saturating_add(storage.bytes_written_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_bytes_written_by_class
            .iter_mut()
            .zip(storage.bytes_written_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_backend_read_ops = self
            .storage_backend_read_ops
            .saturating_add(storage.backend_read_ops_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_backend_read_ops_by_class
            .iter_mut()
            .zip(storage.backend_read_ops_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_backend_bytes_read = self
            .storage_backend_bytes_read
            .saturating_add(storage.backend_bytes_read_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_backend_bytes_read_by_class
            .iter_mut()
            .zip(storage.backend_bytes_read_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_read_latency_ns = self
            .storage_read_latency_ns
            .saturating_add(storage.read_latency_ns_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_read_latency_ns_by_class
            .iter_mut()
            .zip(storage.read_latency_ns_by_class)
        {
            *target = target.saturating_add(value);
        }
        self.storage_write_latency_ns = self
            .storage_write_latency_ns
            .saturating_add(storage.write_latency_ns_by_class.iter().sum::<u64>());
        for (target, value) in self
            .storage_write_latency_ns_by_class
            .iter_mut()
            .zip(storage.write_latency_ns_by_class)
        {
            *target = target.saturating_add(value);
        }
        add_bucket_counts(
            &mut self.storage_read_latency_buckets,
            storage.read_latency_buckets,
        );
        add_bucket_counts(
            &mut self.storage_write_latency_buckets,
            storage.write_latency_buckets,
        );
        self.storage_sync_latency_ns = self
            .storage_sync_latency_ns
            .saturating_add(storage.sync_latency_ns);
        add_bucket_counts(
            &mut self.storage_sync_latency_buckets,
            storage.sync_latency_buckets,
        );
        self.storage_hash_latency_ns = self
            .storage_hash_latency_ns
            .saturating_add(storage.hash_latency_ns);
        add_bucket_counts(
            &mut self.storage_hash_latency_buckets,
            storage.hash_latency_buckets,
        );
        self.storage_sync_ops = self.storage_sync_ops.saturating_add(storage.sync_ops);
        self.storage_hash_ops = self.storage_hash_ops.saturating_add(storage.hash_ops);
        self.storage_preallocation_failures = self
            .storage_preallocation_failures
            .saturating_add(storage.preallocation_failures);
        self.storage_preallocation_fallbacks = self
            .storage_preallocation_fallbacks
            .saturating_add(storage.preallocation_fallbacks);
        self.storage_peer_read_cache_entries = self
            .storage_peer_read_cache_entries
            .saturating_add(storage.peer_read_cache_entries as u64);
        self.storage_peer_read_cache_hits = self
            .storage_peer_read_cache_hits
            .saturating_add(storage.peer_read_cache_hits);
        self.storage_peer_read_cache_misses = self
            .storage_peer_read_cache_misses
            .saturating_add(storage.peer_read_cache_misses);
        self.storage_peer_read_elevator_enabled = self
            .storage_peer_read_elevator_enabled
            .saturating_add(if storage.peer_read_elevator_enabled {
                1
            } else {
                0
            });
        self.storage_peer_read_elevator_queue_depth = self
            .storage_peer_read_elevator_queue_depth
            .saturating_add(storage.peer_read_elevator_queue_depth as u64);
        self.storage_peer_read_elevator_queued = self
            .storage_peer_read_elevator_queued
            .saturating_add(storage.peer_read_elevator_queued as u64);
        self.storage_peer_read_elevator_batches = self
            .storage_peer_read_elevator_batches
            .saturating_add(storage.peer_read_elevator_batches);
        self.storage_peer_read_elevator_coalesced_requests = self
            .storage_peer_read_elevator_coalesced_requests
            .saturating_add(storage.peer_read_elevator_coalesced_requests);
    }
}

fn add_bucket_counts<const N: usize>(target: &mut [u64; N], source: [u64; N]) {
    for (target, value) in target.iter_mut().zip(source) {
        *target = target.saturating_add(value);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use rt_storage::FilePoolStats;

    #[test]
    fn engine_stats_accumulates_torrent_runtime_storage_counters() {
        let mut stats = EngineStats::default();
        let mut storage = StorageIoStats {
            file_pool: FilePoolStats {
                capacity: 64,
                open_files: 8,
                hits: 10,
                misses: 2,
                evictions: 1,
                idle_closes: 3,
            },
            io_queue_depth: 4,
            hash_queue_depth: 5,
            dirty_files: 6,
            sync_ops: 7,
            hash_ops: 8,
            preallocation_failures: 9,
            preallocation_fallbacks: 10,
            peer_read_cache_entries: 11,
            peer_read_cache_hits: 12,
            peer_read_cache_misses: 13,
            peer_read_elevator_enabled: true,
            peer_read_elevator_queue_depth: 14,
            peer_read_elevator_queued: 15,
            peer_read_elevator_batches: 16,
            peer_read_elevator_coalesced_requests: 17,
            ..Default::default()
        };
        storage.read_ops_by_class[0] = 18;
        storage.read_ops_by_class[1] = 19;
        storage.write_ops_by_class[0] = 20;
        storage.bytes_read_by_class[0] = 21;
        storage.bytes_written_by_class[0] = 22;
        storage.backend_read_ops_by_class[4] = 3;
        storage.backend_bytes_read_by_class[4] = 4096;
        storage.read_latency_ns_by_class[4] = 100;
        storage.write_latency_ns_by_class[3] = 200;
        storage.read_latency_buckets[0] = 1;
        storage.write_latency_buckets[1] = 2;
        storage.sync_latency_buckets[2] = 3;
        storage.hash_latency_buckets[3] = 4;
        storage.sync_latency_ns = 300;
        storage.hash_latency_ns = 400;

        stats.add_torrent_runtime(TorrentRuntimeStats {
            piece_assembly_buffers: 19,
            piece_assembly_bytes: 20,
            piece_assembly_evictions: 21,
            storage,
        });

        assert_eq!(stats.storage_file_pool_capacity, 64);
        assert_eq!(stats.storage_file_pool_open_files, 8);
        assert_eq!(stats.storage_file_pool_hits, 10);
        assert_eq!(stats.storage_file_pool_misses, 2);
        assert_eq!(stats.storage_read_ops, 37);
        assert_eq!(stats.storage_write_ops, 20);
        assert_eq!(stats.storage_bytes_read, 21);
        assert_eq!(stats.storage_bytes_written, 22);
        assert_eq!(stats.storage_read_ops_by_class[0], 18);
        assert_eq!(stats.storage_read_ops_by_class[1], 19);
        assert_eq!(stats.storage_write_ops_by_class[0], 20);
        assert_eq!(stats.storage_bytes_read_by_class[0], 21);
        assert_eq!(stats.storage_bytes_written_by_class[0], 22);
        assert_eq!(stats.storage_backend_read_ops, 3);
        assert_eq!(stats.storage_backend_bytes_read, 4096);
        assert_eq!(stats.storage_backend_read_ops_by_class[4], 3);
        assert_eq!(stats.storage_backend_bytes_read_by_class[4], 4096);
        assert_eq!(stats.storage_read_latency_ns, 100);
        assert_eq!(stats.storage_write_latency_ns, 200);
        assert_eq!(stats.storage_read_latency_ns_by_class[4], 100);
        assert_eq!(stats.storage_write_latency_ns_by_class[3], 200);
        assert_eq!(stats.storage_read_latency_buckets[0], 1);
        assert_eq!(stats.storage_write_latency_buckets[1], 2);
        assert_eq!(stats.storage_sync_latency_buckets[2], 3);
        assert_eq!(stats.storage_hash_latency_buckets[3], 4);
        assert_eq!(stats.storage_sync_latency_ns, 300);
        assert_eq!(stats.storage_hash_latency_ns, 400);
        assert_eq!(stats.storage_peer_read_cache_hits, 12);
        assert_eq!(stats.storage_peer_read_elevator_enabled, 1);
        assert_eq!(stats.storage_peer_read_elevator_queue_depth, 14);
        assert_eq!(stats.storage_peer_read_elevator_queued, 15);
        assert_eq!(stats.storage_peer_read_elevator_batches, 16);
        assert_eq!(stats.storage_peer_read_elevator_coalesced_requests, 17);
        assert_eq!(stats.piece_assembly_buffers, 19);
        assert_eq!(stats.piece_assembly_bytes, 20);
        assert_eq!(stats.piece_assembly_evictions, 21);
    }
}
