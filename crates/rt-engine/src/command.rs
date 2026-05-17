/// Commands sent from the API layer down to the engine or individual torrent tasks.
use std::{net::SocketAddr, path::PathBuf};

use tokio::sync::oneshot;

use rt_metainfo::{MagnetLink, TorrentMeta};
use rt_metrics::{MemoryClass, MemoryLease, ResourceSnapshot};
use rt_storage::{StorageIoStats, STORAGE_LATENCY_BUCKET_COUNT};

use crate::TorrentActivityTier;

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
    pub torrents_activity_hot: u64,
    pub torrents_activity_warm: u64,
    pub torrents_activity_dormant: u64,
    pub torrent_tasks_active: u64,
    pub fastresume_dirty_pieces: u64,
    pub completed_piece_verify_from_memory: u64,
    pub completed_piece_verify_from_disk: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub bytes_left: u64,
    pub jobs_active: u64,
    pub trackers_total: u64,
    pub trackers_working: u64,
    pub trackers_warning: u64,
    pub trackers_error: u64,
    pub dht_routing_nodes: u64,
    pub dht_announced_peer_sets: u64,
    pub dht_announced_peers: u64,
    pub dht_tracked_torrents: u64,
    pub dht_outstanding_requests: u64,
    pub dht_queried_nodes: u64,
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
    pub storage_device_latencies: Vec<StorageDeviceLatencyStats>,
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
    pub storage_page_cache_advise_sequential: u64,
    pub storage_page_cache_advise_willneed: u64,
    pub storage_page_cache_advise_dontneed: u64,
    pub storage_page_cache_advise_failures: u64,
    pub storage_sparse_data_extents: u64,
    pub storage_sparse_hole_bytes: u64,
    pub storage_sparse_seek_fallbacks: u64,
    pub piece_assembly_buffers: u64,
    pub piece_assembly_bytes: u64,
    pub piece_assembly_evictions: u64,
    pub peer_request_window_reductions: u64,
    pub peer_rx_buffer_bytes: u64,
    pub peer_tx_buffer_bytes: u64,
    pub peer_command_queue_depth: u64,
    pub peer_command_queue_capacity: u64,
    pub peer_command_queue_full: u64,
    pub tracker_peer_cache_entries: u64,
    pub tracker_peer_cache_drops: u64,
    pub hot_torrent_memory_top: Vec<HotTorrentMemoryStats>,
    pub resources: Option<ResourceSnapshot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotTorrentMemoryStats {
    pub info_hash: String,
    pub estimated_bytes: u64,
    pub piece_assembly_bytes: u64,
    pub peer_buffer_bytes: u64,
    pub tracker_peer_bytes: u64,
    pub peer_command_queue_bytes: u64,
    pub storage_cache_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageDeviceLatencyStats {
    pub device_id: String,
    pub profile: String,
    pub read_latency_ns: u64,
    pub write_latency_ns: u64,
    pub sync_latency_ns: u64,
    pub hash_latency_ns: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TorrentRuntimeStats {
    pub connected_peers: u64,
    pub outstanding_requests: u64,
    pub fastresume_dirty_pieces: u64,
    pub completed_piece_verify_from_memory: u64,
    pub completed_piece_verify_from_disk: u64,
    pub piece_assembly_buffers: u64,
    pub piece_assembly_bytes: u64,
    pub piece_assembly_evictions: u64,
    pub peer_request_window_reductions: u64,
    pub peer_rx_buffer_bytes: u64,
    pub peer_tx_buffer_bytes: u64,
    pub peer_command_queue_depth: u64,
    pub peer_command_queue_capacity: u64,
    pub peer_command_queue_full: u64,
    pub tracker_peer_cache_entries: u64,
    pub tracker_peer_cache_drops: u64,
    pub storage: StorageIoStats,
}

impl EngineStats {
    const HOT_TORRENT_MEMORY_TOP_N: usize = 10;

    pub fn add_activity_tier(&mut self, tier: TorrentActivityTier) {
        match tier {
            TorrentActivityTier::Hot => self.torrents_activity_hot += 1,
            TorrentActivityTier::Warm => self.torrents_activity_warm += 1,
            TorrentActivityTier::Dormant => self.torrents_activity_dormant += 1,
        }
    }

    pub fn add_torrent_runtime(&mut self, info_hash: String, runtime: TorrentRuntimeStats) {
        self.record_hot_torrent_memory(info_hash, &runtime);
        self.torrent_tasks_active = self.torrent_tasks_active.saturating_add(1);
        self.fastresume_dirty_pieces = self
            .fastresume_dirty_pieces
            .saturating_add(runtime.fastresume_dirty_pieces);
        self.completed_piece_verify_from_memory = self
            .completed_piece_verify_from_memory
            .saturating_add(runtime.completed_piece_verify_from_memory);
        self.completed_piece_verify_from_disk = self
            .completed_piece_verify_from_disk
            .saturating_add(runtime.completed_piece_verify_from_disk);
        self.piece_assembly_buffers = self
            .piece_assembly_buffers
            .saturating_add(runtime.piece_assembly_buffers);
        self.piece_assembly_bytes = self
            .piece_assembly_bytes
            .saturating_add(runtime.piece_assembly_bytes);
        self.piece_assembly_evictions = self
            .piece_assembly_evictions
            .saturating_add(runtime.piece_assembly_evictions);
        self.peer_request_window_reductions = self
            .peer_request_window_reductions
            .saturating_add(runtime.peer_request_window_reductions);
        self.peer_rx_buffer_bytes = self
            .peer_rx_buffer_bytes
            .saturating_add(runtime.peer_rx_buffer_bytes);
        self.peer_tx_buffer_bytes = self
            .peer_tx_buffer_bytes
            .saturating_add(runtime.peer_tx_buffer_bytes);
        self.peer_command_queue_depth = self
            .peer_command_queue_depth
            .saturating_add(runtime.peer_command_queue_depth);
        self.peer_command_queue_capacity = self
            .peer_command_queue_capacity
            .saturating_add(runtime.peer_command_queue_capacity);
        self.peer_command_queue_full = self
            .peer_command_queue_full
            .saturating_add(runtime.peer_command_queue_full);
        self.tracker_peer_cache_entries = self
            .tracker_peer_cache_entries
            .saturating_add(runtime.tracker_peer_cache_entries);
        self.tracker_peer_cache_drops = self
            .tracker_peer_cache_drops
            .saturating_add(runtime.tracker_peer_cache_drops);

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
        let device_id = storage
            .device_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let profile = storage_profile_label(&storage.profile).to_string();
        let device = match self
            .storage_device_latencies
            .iter_mut()
            .find(|device| device.device_id == device_id && device.profile == profile)
        {
            Some(device) => device,
            None => {
                self.storage_device_latencies
                    .push(StorageDeviceLatencyStats {
                        device_id,
                        profile,
                        ..Default::default()
                    });
                self.storage_device_latencies
                    .last_mut()
                    .expect("just inserted device latency stats")
            }
        };
        device.read_latency_ns = device
            .read_latency_ns
            .saturating_add(storage.read_latency_ns_by_class.iter().sum::<u64>());
        device.write_latency_ns = device
            .write_latency_ns
            .saturating_add(storage.write_latency_ns_by_class.iter().sum::<u64>());
        device.sync_latency_ns = device
            .sync_latency_ns
            .saturating_add(storage.sync_latency_ns);
        device.hash_latency_ns = device
            .hash_latency_ns
            .saturating_add(storage.hash_latency_ns);
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
        self.storage_page_cache_advise_sequential = self
            .storage_page_cache_advise_sequential
            .saturating_add(storage.page_cache_advise_sequential);
        self.storage_page_cache_advise_willneed = self
            .storage_page_cache_advise_willneed
            .saturating_add(storage.page_cache_advise_willneed);
        self.storage_page_cache_advise_dontneed = self
            .storage_page_cache_advise_dontneed
            .saturating_add(storage.page_cache_advise_dontneed);
        self.storage_page_cache_advise_failures = self
            .storage_page_cache_advise_failures
            .saturating_add(storage.page_cache_advise_failures);
        self.storage_sparse_data_extents = self
            .storage_sparse_data_extents
            .saturating_add(storage.sparse_data_extents);
        self.storage_sparse_hole_bytes = self
            .storage_sparse_hole_bytes
            .saturating_add(storage.sparse_hole_bytes);
        self.storage_sparse_seek_fallbacks = self
            .storage_sparse_seek_fallbacks
            .saturating_add(storage.sparse_seek_fallbacks);
    }

    fn record_hot_torrent_memory(&mut self, info_hash: String, runtime: &TorrentRuntimeStats) {
        let peer_buffer_bytes = runtime
            .peer_rx_buffer_bytes
            .saturating_add(runtime.peer_tx_buffer_bytes);
        let tracker_peer_bytes = runtime.tracker_peer_cache_entries.saturating_mul(64);
        let peer_command_queue_bytes = runtime.peer_command_queue_capacity.saturating_mul(64);
        let storage_cache_bytes = (runtime.storage.file_pool.open_files as u64).saturating_mul(256);
        let estimated_bytes = runtime
            .piece_assembly_bytes
            .saturating_add(peer_buffer_bytes)
            .saturating_add(tracker_peer_bytes)
            .saturating_add(peer_command_queue_bytes)
            .saturating_add(storage_cache_bytes);
        if estimated_bytes == 0 {
            return;
        }
        self.hot_torrent_memory_top.push(HotTorrentMemoryStats {
            info_hash,
            estimated_bytes,
            piece_assembly_bytes: runtime.piece_assembly_bytes,
            peer_buffer_bytes,
            tracker_peer_bytes,
            peer_command_queue_bytes,
            storage_cache_bytes,
        });
        self.hot_torrent_memory_top.sort_by(|a, b| {
            b.estimated_bytes
                .cmp(&a.estimated_bytes)
                .then_with(|| a.info_hash.cmp(&b.info_hash))
        });
        self.hot_torrent_memory_top
            .truncate(Self::HOT_TORRENT_MEMORY_TOP_N);
    }
}

fn storage_profile_label(profile: &rt_path::StorageProfile) -> &'static str {
    match profile {
        rt_path::StorageProfile::Hdd => "hdd",
        rt_path::StorageProfile::Ssd => "ssd",
        rt_path::StorageProfile::Nvme => "nvme",
        rt_path::StorageProfile::Network => "network",
        rt_path::StorageProfile::Unknown => "unknown",
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
    /// Reserve governor-managed memory for bounded API materialization.
    ReserveMemory {
        class: MemoryClass,
        bytes: u64,
        reply: oneshot::Sender<CmdResult<Option<MemoryLease>>>,
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
            device_id: Some("nvme0n1".to_string()),
            profile: rt_path::StorageProfile::Nvme,
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
            page_cache_advise_sequential: 18,
            page_cache_advise_willneed: 19,
            page_cache_advise_dontneed: 20,
            page_cache_advise_failures: 21,
            sparse_data_extents: 22,
            sparse_hole_bytes: 23,
            sparse_seek_fallbacks: 24,
            ..Default::default()
        };
        storage.read_ops_by_class[0] = 25;
        storage.read_ops_by_class[1] = 26;
        storage.write_ops_by_class[0] = 27;
        storage.bytes_read_by_class[0] = 28;
        storage.bytes_written_by_class[0] = 29;
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

        stats.add_torrent_runtime(
            "hash-a".to_string(),
            TorrentRuntimeStats {
                connected_peers: 2,
                outstanding_requests: 3,
                fastresume_dirty_pieces: 4,
                completed_piece_verify_from_memory: 5,
                completed_piece_verify_from_disk: 6,
                piece_assembly_buffers: 19,
                piece_assembly_bytes: 20,
                piece_assembly_evictions: 21,
                peer_request_window_reductions: 22,
                peer_rx_buffer_bytes: 23,
                peer_tx_buffer_bytes: 24,
                peer_command_queue_depth: 27,
                peer_command_queue_capacity: 28,
                peer_command_queue_full: 29,
                tracker_peer_cache_entries: 25,
                tracker_peer_cache_drops: 26,
                storage,
            },
        );

        assert_eq!(stats.storage_file_pool_capacity, 64);
        assert_eq!(stats.storage_file_pool_open_files, 8);
        assert_eq!(stats.storage_file_pool_hits, 10);
        assert_eq!(stats.storage_file_pool_misses, 2);
        assert_eq!(stats.storage_read_ops, 51);
        assert_eq!(stats.storage_write_ops, 27);
        assert_eq!(stats.storage_bytes_read, 28);
        assert_eq!(stats.storage_bytes_written, 29);
        assert_eq!(stats.storage_read_ops_by_class[0], 25);
        assert_eq!(stats.storage_read_ops_by_class[1], 26);
        assert_eq!(stats.storage_write_ops_by_class[0], 27);
        assert_eq!(stats.storage_bytes_read_by_class[0], 28);
        assert_eq!(stats.storage_bytes_written_by_class[0], 29);
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
        assert_eq!(
            stats.storage_device_latencies,
            vec![StorageDeviceLatencyStats {
                device_id: "nvme0n1".to_string(),
                profile: "nvme".to_string(),
                read_latency_ns: 100,
                write_latency_ns: 200,
                sync_latency_ns: 300,
                hash_latency_ns: 400,
            }]
        );
        assert_eq!(stats.storage_peer_read_cache_hits, 12);
        assert_eq!(stats.storage_peer_read_elevator_enabled, 1);
        assert_eq!(stats.storage_peer_read_elevator_queue_depth, 14);
        assert_eq!(stats.storage_peer_read_elevator_queued, 15);
        assert_eq!(stats.storage_peer_read_elevator_batches, 16);
        assert_eq!(stats.storage_peer_read_elevator_coalesced_requests, 17);
        assert_eq!(stats.storage_page_cache_advise_sequential, 18);
        assert_eq!(stats.storage_page_cache_advise_willneed, 19);
        assert_eq!(stats.storage_page_cache_advise_dontneed, 20);
        assert_eq!(stats.storage_page_cache_advise_failures, 21);
        assert_eq!(stats.storage_sparse_data_extents, 22);
        assert_eq!(stats.storage_sparse_hole_bytes, 23);
        assert_eq!(stats.storage_sparse_seek_fallbacks, 24);
        assert_eq!(stats.torrent_tasks_active, 1);
        assert_eq!(stats.fastresume_dirty_pieces, 4);
        assert_eq!(stats.completed_piece_verify_from_memory, 5);
        assert_eq!(stats.completed_piece_verify_from_disk, 6);
        assert_eq!(stats.piece_assembly_buffers, 19);
        assert_eq!(stats.piece_assembly_bytes, 20);
        assert_eq!(stats.piece_assembly_evictions, 21);
        assert_eq!(stats.peer_request_window_reductions, 22);
        assert_eq!(stats.peer_rx_buffer_bytes, 23);
        assert_eq!(stats.peer_tx_buffer_bytes, 24);
        assert_eq!(stats.peer_command_queue_depth, 27);
        assert_eq!(stats.peer_command_queue_capacity, 28);
        assert_eq!(stats.peer_command_queue_full, 29);
        assert_eq!(stats.tracker_peer_cache_entries, 25);
        assert_eq!(stats.tracker_peer_cache_drops, 26);
        assert_eq!(
            stats.hot_torrent_memory_top,
            vec![HotTorrentMemoryStats {
                info_hash: "hash-a".to_string(),
                estimated_bytes: 5_507,
                piece_assembly_bytes: 20,
                peer_buffer_bytes: 47,
                tracker_peer_bytes: 1_600,
                peer_command_queue_bytes: 1_792,
                storage_cache_bytes: 2_048,
            }]
        );
    }

    #[test]
    fn engine_stats_tracks_top_hot_torrent_memory_estimates() {
        let mut stats = EngineStats::default();

        for n in 0..12 {
            stats.add_torrent_runtime(
                format!("hash-{n:02}"),
                TorrentRuntimeStats {
                    piece_assembly_bytes: n * 1_024,
                    peer_rx_buffer_bytes: n * 64,
                    peer_tx_buffer_bytes: n * 32,
                    peer_command_queue_capacity: n,
                    tracker_peer_cache_entries: 1,
                    ..Default::default()
                },
            );
        }

        assert_eq!(stats.hot_torrent_memory_top.len(), 10);
        assert_eq!(stats.hot_torrent_memory_top[0].info_hash, "hash-11");
        assert_eq!(stats.hot_torrent_memory_top[1].info_hash, "hash-10");
        assert_eq!(stats.hot_torrent_memory_top[9].info_hash, "hash-02");
        assert!(stats
            .hot_torrent_memory_top
            .windows(2)
            .all(|window| window[0].estimated_bytes >= window[1].estimated_bytes));
    }
}
