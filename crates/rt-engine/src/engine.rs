use std::collections::{HashMap, HashSet};
/// Top-level engine: manages torrent task lifecycle and incoming peer listeners.
use std::future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Context;
use futures::{stream, StreamExt};
use rusqlite::Connection;
use sha1::{Digest, Sha1};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

use rt_config::Config;
use rt_db::TorrentRow;
use rt_fastresume::{FastresumeStore, PieceState};
use rt_metainfo::{parse_torrent, MagnetLink, TorrentMeta, TorrentMetaV1, TorrentMetaV2};
use rt_metrics::{
    MemoryClass, MemoryPressure, ResourceGovernor, ResourceGovernorConfig, MEMORY_CLASS_COUNT,
};
use rt_path::{StorageProfile, StorageRootId};
use rt_peer_wire::handshake::{Handshake, HANDSHAKE_LEN};
use rt_session::{SessionRegistry, TorrentEntry, TorrentState, TransferStats};
use rt_storage::{
    runtime::StorageRuntime, DurabilityMode, MountScheduler, PreallocationMode, SchedulerConfig,
    StorageError, StorageIoConfig, StoragePlan, StoragePlanStep, V2FileHash, V2FileVerifier,
    VerifyResult,
};
use rt_utp::{UtpEndpoint, UtpStream};

use crate::command::{
    CmdResult, EngineCmd, EngineGlobalLimits, EngineJob, EngineNetworkFeatures, EnginePeerSnapshot,
    EnginePieceState, EngineStats, EngineStorageRoot, EngineSubsystemHealth, EngineTorrentFile,
    EngineTorrentLimits, EngineTorrentMetadata, EngineTrackerSnapshot, EngineWebseedSnapshot,
    QueueMove, TorrentDiagnostic,
};
use crate::dht_task::{run_dht, DhtCommand, DhtTorrent};
use crate::egress_policy::OutboundEgressPolicy;
use crate::metadata_task::run_metadata_task;
use crate::network_budget::GlobalNetworkBudget;
use crate::peer_ingress::{PeerIngressBudget, PeerIngressConfig, PeerIngressPermit};
use crate::storage_authority::ServerStorageRoots;
use crate::storage_jobs::{StorageJobAction, StorageJobCompletion, StorageJobDispatcher};
use crate::tier::{
    CompactPieceBitmap, DormantTorrentSnapshot, TierController, TierEvent, TierInput, TierPolicy,
};
use crate::torrent_task::{TorrentCmd, TorrentTask};

const EVENT_ENGINE_STARTED: &str = "engine_started";
const EVENT_TORRENT_ADDED: &str = "torrent_added";
const EVENT_MAGNET_ADDED: &str = "magnet_added";
const EVENT_METADATA_RESOLVED: &str = "metadata_resolved";
const EVENT_TORRENT_RESTORED: &str = "torrent_restored";
const EVENT_TORRENT_REMOVED: &str = "torrent_removed";
const EVENT_TORRENT_PAUSED: &str = "torrent_paused";
const EVENT_TORRENT_RESUMED: &str = "torrent_resumed";
const EVENT_RECHECK_REQUESTED: &str = "check_requested";
const EVENT_REANNOUNCE_REQUESTED: &str = "tracker_reannounce_requested";
const EVENT_LABELS_UPDATED: &str = "labels_updated";
const EVENT_FIELDS_UPDATED: &str = "torrent_fields_updated";
const EVENT_TRACKERS_UPDATED: &str = "trackers_updated";
const EVENT_LIMITS_UPDATED: &str = "limits_updated";
const EVENT_ENGINE_STOPPED: &str = "engine_stopped";

const JOB_KIND_RECHECK: &str = "recheck_torrent";
const JOB_KIND_STORAGE_PLAN: &str = "storage_plan";
const JOB_STATE_QUEUED: &str = "queued";
const JOB_STATE_RUNNING: &str = "running";
const JOB_STATE_PAUSED: &str = "paused";
const JOB_STATE_CANCELLED: &str = "cancelled";
const JOB_STATE_FAILED: &str = "failed";
const JOB_STATE_COMPLETED: &str = "completed";
static RECHECK_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static STORAGE_PLAN_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const SETTING_GLOBAL_DOWNLOAD_LIMIT: &str = "transfer.download_limit";
const SETTING_GLOBAL_UPLOAD_LIMIT: &str = "transfer.upload_limit";
const SETTING_GLOBAL_SPEED_LIMITS_MODE: &str = "transfer.speed_limits_mode";
const SETTING_NETWORK_DHT: &str = "network.dht";
const SETTING_NETWORK_PEX: &str = "network.pex";
const SETTING_QUEUE_PREFIX: &str = "torrent.queue.";
const ENGINE_STATS_CACHE_TTL: Duration = Duration::from_millis(500);

fn resource_config_from_config(config: &Config) -> ResourceGovernorConfig {
    let mib = 1024 * 1024;
    let mut class_caps_bytes = [0; MEMORY_CLASS_COUNT];
    class_caps_bytes[MemoryClass::StorageFrame as usize] =
        config.memory.storage_frame_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::PieceAssembly as usize] =
        config.memory.piece_assembly_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::PeerBuffer as usize] =
        config.memory.peer_buffer_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::WebseedBody as usize] =
        config.memory.peer_buffer_cap_mb.saturating_mul(mib) / 2;
    class_caps_bytes[MemoryClass::Metadata as usize] =
        config.memory.metadata_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::TrackerPeers as usize] =
        config.memory.metadata_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::DhtTable as usize] =
        config.memory.metadata_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::QueuedDisk as usize] =
        config.memory.queued_disk_cap_mb.saturating_mul(mib);
    class_caps_bytes[MemoryClass::ApiSnapshot as usize] =
        config.memory.metadata_cap_mb.saturating_mul(mib) / 2;
    ResourceGovernorConfig {
        total_cap_bytes: config.memory.total_cap_mb.saturating_mul(mib),
        class_caps_bytes,
        pressure_constrained_pct: config.memory.pressure_constrained_pct,
        pressure_critical_pct: config.memory.pressure_critical_pct,
    }
}

fn storage_io_config_from_config(config: &Config) -> StorageIoConfig {
    StorageIoConfig {
        file_pool_size: config.storage.file_pool_size,
        idle_file_ttl_secs: config.storage.idle_file_ttl_secs,
        io_worker_threads: config.storage.io_worker_threads,
        io_queue_depth: config.storage.io_queue_depth,
        hash_worker_threads: config.storage.hash_worker_threads,
        hash_queue_depth: config.storage.hash_queue_depth,
        preallocation_mode: match config.storage.preallocation_mode {
            rt_config::StoragePreallocationMode::Off => PreallocationMode::Off,
            rt_config::StoragePreallocationMode::Auto => PreallocationMode::Auto,
            rt_config::StoragePreallocationMode::Sparse => PreallocationMode::Sparse,
            rt_config::StoragePreallocationMode::Full => PreallocationMode::Full,
        },
        durability_mode: match config.storage.durability_mode {
            rt_config::StorageDurabilityMode::Fast => DurabilityMode::Fast,
            rt_config::StorageDurabilityMode::Checkpoint => DurabilityMode::Checkpoint,
            rt_config::StorageDurabilityMode::Strict => DurabilityMode::Strict,
        },
        peer_read_readahead_bytes: config.storage.peer_read_readahead_bytes,
        peer_read_cache_entries: config.storage.peer_read_cache_entries,
        peer_read_elevator_budget_ms: if config.storage.device_elevator_enabled {
            config.storage.peer_read_elevator_budget_ms
        } else {
            0
        },
    }
}

fn spawn_dht_task(config: &Config) -> mpsc::Sender<DhtCommand> {
    let (dht_tx, dht_rx) = mpsc::channel(64);
    let dht_port = config.dht_port();
    let listen_port = config.network.listen_port;
    let bootstrap_nodes = config.dht.bootstrap_nodes.clone();
    tokio::spawn(async move {
        if let Err(e) = run_dht(dht_port, listen_port, bootstrap_nodes, dht_rx).await {
            warn!(
                component = "dht",
                operation = "run",
                result = "error",
                error = %e,
                "DHT task exited with error"
            );
        }
    });
    dht_tx
}

async fn shutdown_dht_task(tx: mpsc::Sender<DhtCommand>, timeout_budget: Duration) {
    let (reply, rx) = oneshot::channel();
    if tx.send(DhtCommand::Shutdown { reply }).await.is_err() {
        return;
    }
    if timeout(timeout_budget, rx).await.is_err() {
        warn!(
            component = "dht",
            operation = "shutdown",
            result = "timeout",
            "DHT task did not acknowledge shutdown before deadline"
        );
    }
}

fn memory_pressure_for(
    used: u64,
    cap: u64,
    constrained_pct: u8,
    critical_pct: u8,
) -> MemoryPressure {
    if cap == 0 {
        return MemoryPressure::Critical;
    }
    let used_pct = used.saturating_mul(100) / cap;
    if used_pct >= critical_pct as u64 {
        MemoryPressure::Critical
    } else if used_pct >= constrained_pct as u64 {
        MemoryPressure::Constrained
    } else {
        MemoryPressure::Normal
    }
}

/// Handle given to the API layer. Clone freely; all sends are channel-based.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineCmd>,
}

impl EngineHandle {
    /// Return whether the engine actor still owns its command receiver.
    /// This is a cheap death check for readiness probes; it intentionally
    /// does not send a command or wait behind a slow actor.
    pub fn is_alive(&self) -> bool {
        !self.tx.is_closed()
    }

    /// Add a torrent; returns the v1 info_hash hex string.
    pub async fn add_torrent(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
    ) -> CmdResult<String> {
        self.add_torrent_with_labels(meta, save_path, paused, None, Vec::new())
            .await
    }

    pub async fn add_torrent_with_labels(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddTorrent {
                meta: Box::new(meta),
                save_path,
                paused,
                category,
                tags,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn add_magnet_with_labels(
        &self,
        magnet: MagnetLink,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddMagnet {
                magnet,
                save_path,
                paused,
                category,
                tags,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn remove_torrent(&self, info_hash: String, delete_files: bool) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RemoveTorrent {
                info_hash,
                delete_files,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn pause_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::PauseTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn resume_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ResumeTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn recheck_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RecheckTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn pause_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::PauseJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn resume_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ResumeJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn cancel_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::CancelJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn reannounce_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ReannounceTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_metadata(&self, info_hash: String) -> CmdResult<EngineTorrentMetadata> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentMetadata { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_blob(&self, info_hash: String) -> CmdResult<Vec<u8>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentBlob { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_trackers(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineTrackerSnapshot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentTrackers { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn execute_storage_plan(
        &self,
        operation: String,
        affected_torrents: Vec<String>,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ExecuteStoragePlan {
                operation,
                affected_torrents,
                plan,
                completed_steps,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn list_jobs(&self) -> CmdResult<Vec<EngineJob>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ListJobs { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn list_storage_roots(&self) -> CmdResult<Vec<EngineStorageRoot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ListStorageRoots { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn configured_storage_roots(&self) -> CmdResult<Vec<PathBuf>> {
        let roots = self.list_storage_roots().await?;
        ServerStorageRoots::from_configured_paths(
            roots.into_iter().map(|root| root.path).collect::<Vec<_>>(),
        )
        .map(ServerStorageRoots::into_roots)
        .map_err(|error| error.to_string())
    }

    pub async fn stats(&self) -> CmdResult<EngineStats> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetStats { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetHealth { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn session_events(
        &self,
        info_hash: Option<String>,
        limit: usize,
    ) -> CmdResult<Vec<rt_db::SessionEventRow>> {
        self.session_events_filtered(info_hash, None, Vec::new(), None, limit)
            .await
    }

    pub async fn session_events_filtered(
        &self,
        info_hash: Option<String>,
        kind: Option<String>,
        levels: Vec<String>,
        last_known_id: Option<i64>,
        limit: usize,
    ) -> CmdResult<Vec<rt_db::SessionEventRow>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ListSessionEvents {
                info_hash,
                kind,
                levels,
                last_known_id,
                limit,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn reserve_memory(
        &self,
        class: MemoryClass,
        bytes: u64,
    ) -> CmdResult<Option<rt_metrics::MemoryLease>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ReserveMemory {
                class,
                bytes,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn diagnose_torrent(&self, info_hash: String) -> CmdResult<TorrentDiagnostic> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::DiagnoseTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_labels(
        &self,
        info_hash: String,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentLabels {
                info_hash,
                category,
                add_tags,
                remove_tags,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_fields(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentFields {
                info_hash,
                name,
                save_path,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_fields_with_job(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentFieldsWithJob {
                info_hash,
                name,
                save_path,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_trackers(
        &self,
        info_hash: String,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentTrackers {
                info_hash,
                trackers,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_limits(&self, info_hash: String) -> CmdResult<EngineTorrentLimits> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentLimits { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_limits(
        &self,
        info_hash: String,
        limits: EngineTorrentLimits,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentLimits {
                info_hash,
                limits,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_file_priorities(
        &self,
        info_hash: String,
        file_ids: Vec<u32>,
        priority: i64,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateFilePriorities {
                info_hash,
                file_ids,
                priority,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn rename_file_path(
        &self,
        info_hash: String,
        file_id: u32,
        new_path: String,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RenameFilePath {
                info_hash,
                file_id,
                new_path,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn rename_folder_path(
        &self,
        info_hash: String,
        old_path: String,
        new_path: String,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RenameFolderPath {
                info_hash,
                old_path,
                new_path,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn add_peers(&self, info_hash: String, peers: Vec<SocketAddr>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddPeers {
                info_hash,
                peers,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_peers(&self, info_hash: String) -> CmdResult<Vec<EnginePeerSnapshot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentPeers { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_webseeds(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineWebseedSnapshot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentWebseeds { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn global_limits(&self) -> CmdResult<EngineGlobalLimits> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetGlobalLimits { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_global_limits(&self, limits: EngineGlobalLimits) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateGlobalLimits { limits, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn network_features(&self) -> CmdResult<EngineNetworkFeatures> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetNetworkFeatures { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_network_features(&self, features: EngineNetworkFeatures) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateNetworkFeatures { features, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn queue_priority(&self, info_hash: String) -> CmdResult<i32> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetQueuePriority { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_queue_order(
        &self,
        info_hashes: Vec<String>,
        queue_move: QueueMove,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateQueueOrder {
                info_hashes,
                queue_move,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(EngineCmd::Shutdown { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// The running engine.
pub struct Engine {
    config: Arc<Config>,
    registry: Arc<RwLock<SessionRegistry>>,
    db: Arc<Mutex<Connection>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    cmd_tx: mpsc::Sender<EngineCmd>,
    /// info_hash_hex → channel to the torrent task
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    torrent_tasks: HashMap<String, JoinHandle<()>>,
    dht_tx: Option<mpsc::Sender<DhtCommand>>,
    resources: ResourceGovernor,
    network_budget: GlobalNetworkBudget,
    storage_jobs: StorageJobDispatcher,
    tier_controller: crate::tier::TierController<String>,
    tier_last_active: HashMap<String, Instant>,
    stats_cache: Option<(Instant, EngineStats)>,
    shutdown_reply: Option<oneshot::Sender<()>>,
}

impl Engine {
    /// Spawn the engine, returning an EngineHandle for the API layer.
    pub async fn start(
        config: Arc<Config>,
        registry: Arc<RwLock<SessionRegistry>>,
    ) -> anyhow::Result<EngineHandle> {
        let (tx, cmd_rx) = mpsc::channel(64);
        let handle = EngineHandle { tx: tx.clone() };
        std::fs::create_dir_all(&config.daemon.session_dir)
            .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;
        std::fs::create_dir_all(torrent_blob_dir(&config))
            .with_context(|| "creating torrent metadata directory")?;
        std::fs::create_dir_all(fastresume_dir(&config))
            .with_context(|| "creating fastresume directory")?;
        let conn = Connection::open(config.db_path())
            .with_context(|| format!("opening database {:?}", config.db_path()))?;
        rt_db::migrate(&conn).context("migrating database")?;
        register_configured_storage(&conn, &config).context("registering configured storage")?;
        let db = Arc::new(Mutex::new(conn));
        let storage_jobs = StorageJobDispatcher::new(Arc::clone(&db));

        let dht_enabled = {
            let conn = db.lock().expect("database mutex poisoned");
            setting_bool_with_default(&conn, SETTING_NETWORK_DHT, config.dht.enabled)
        };
        let dht_shutdown = if dht_enabled {
            Some(spawn_dht_task(&config))
        } else {
            None
        };

        let network_budget = GlobalNetworkBudget::new(
            config.network.max_peers,
            (config.network.download_rate_limit > 0).then_some(config.network.download_rate_limit),
            (config.network.upload_rate_limit > 0).then_some(config.network.upload_rate_limit),
        );
        let mut engine = Engine {
            config: config.clone(),
            registry,
            db,
            cmd_rx,
            cmd_tx: tx,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: dht_shutdown,
            resources: ResourceGovernor::new(resource_config_from_config(&config)),
            network_budget,
            storage_jobs,
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        engine.apply_shared_global_limits_from_db();
        engine.append_session_event(
            None,
            EVENT_ENGINE_STARTED,
            Some("native engine started"),
            serde_json::json!({
                "listen_port": config.network.listen_port,
                "dht_enabled": dht_enabled,
            }),
        );
        engine.recover_interrupted_jobs()?;
        engine.load_persisted_torrents().await?;
        engine.resume_recovered_storage_jobs().await?;

        // Spawn TCP listener.
        let listen_addr: SocketAddr = format!("0.0.0.0:{}", config.network.listen_port)
            .parse()
            .context("invalid listen_port")?;
        let listener = TcpListener::bind(listen_addr).await?;
        info!(
            component = "peer_listener",
            operation = "listen",
            addr = %listen_addr,
            "TCP peer listener bound"
        );
        let utp_endpoint = if incoming_utp_enabled() {
            let endpoint = UtpEndpoint::bind(listen_addr)
                .await
                .context("binding uTP peer listener")?;
            info!(
                component = "peer_listener",
                operation = "listen_utp",
                addr = %listen_addr,
                "uTP peer listener bound"
            );
            Some(endpoint)
        } else {
            None
        };

        let peer_ingress = Arc::new(PeerIngressBudget::new(PeerIngressConfig {
            max_global_handshakes: config.network.max_incoming_handshakes,
            max_handshakes_per_ip: config.network.max_incoming_handshakes_per_ip,
            per_ip_window: Duration::from_secs(config.network.incoming_handshake_window_secs),
            handshake_timeout: Duration::from_secs(config.network.incoming_handshake_timeout_secs),
        }));
        tokio::spawn(engine.run(listener, utp_endpoint, peer_ingress));
        Ok(handle)
    }

    async fn run(
        mut self,
        listener: TcpListener,
        utp_endpoint: Option<UtpEndpoint>,
        peer_ingress: Arc<PeerIngressBudget>,
    ) {
        let mut tier_tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    if !self.handle_cmd(cmd).await {
                        break;
                    }
                }
                Ok((stream, peer_addr)) = listener.accept() => {
                    // Incoming peer connection — hand off to a task that
                    // reads the handshake and routes to the right torrent task.
                    match peer_ingress.try_begin(peer_addr, Instant::now()) {
                        Ok(permit) => {
                            let Ok(peer_permit) = self.network_budget.try_acquire_peer() else {
                                warn!(
                                    component = "peer_listener",
                                    operation = "accept_peer",
                                    peer = %peer_addr,
                                    result = "rejected",
                                    reason = "global peer connection budget",
                                    "incoming peer rejected by global connection budget"
                                );
                                continue;
                            };
                            let chans = self.torrent_chans.clone();
                            let engine_tx = self.cmd_tx.clone();
                            let handshake_timeout = peer_ingress.config().handshake_timeout;
                            tokio::spawn(async move {
                                if let Err(e) = handle_incoming(
                                    stream,
                                    peer_addr,
                                    chans,
                                    engine_tx,
                                    permit,
                                    peer_permit,
                                    handshake_timeout,
                                )
                                .await
                                {
                                    warn!(
                                        component = "peer_listener",
                                        operation = "accept_peer",
                                        peer = %peer_addr,
                                        result = "error",
                                        error = %e,
                                        "incoming peer error"
                                    );
                                }
                            });
                        }
                        Err(e) => {
                            warn!(
                                component = "peer_listener",
                                operation = "accept_peer",
                                peer = %peer_addr,
                                result = "rejected",
                                reason = %e,
                                "incoming peer rejected by handshake budget"
                            );
                        }
                    }
                }
                utp_result = accept_utp_peer(utp_endpoint.as_ref()) => {
                    match utp_result {
                        Ok((stream, peer_addr)) => {
                            match peer_ingress.try_begin(peer_addr, Instant::now()) {
                                Ok(permit) => {
                                    let Ok(peer_permit) = self.network_budget.try_acquire_peer()
                                    else {
                                        warn!(
                                            component = "peer_listener",
                                            operation = "accept_utp_peer",
                                            peer = %peer_addr,
                                            result = "rejected",
                                            reason = "global peer connection budget",
                                            "incoming uTP peer rejected by global connection budget"
                                        );
                                        continue;
                                    };
                                    let chans = self.torrent_chans.clone();
                                    let engine_tx = self.cmd_tx.clone();
                                    let handshake_timeout =
                                        peer_ingress.config().handshake_timeout;
                                    tokio::spawn(async move {
                                        if let Err(e) = handle_incoming_utp(
                                            stream,
                                            peer_addr,
                                            chans,
                                            engine_tx,
                                            permit,
                                            peer_permit,
                                            handshake_timeout,
                                        )
                                        .await
                                        {
                                            warn!(
                                                component = "peer_listener",
                                                operation = "accept_utp_peer",
                                                peer = %peer_addr,
                                                result = "error",
                                                error = %e,
                                                "incoming uTP peer error"
                                            );
                                        }
                                    });
                                }
                                Err(e) => {
                                    warn!(
                                        component = "peer_listener",
                                        operation = "accept_utp_peer",
                                        peer = %peer_addr,
                                        result = "rejected",
                                        reason = %e,
                                        "incoming uTP peer rejected by handshake budget"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                component = "peer_listener",
                                operation = "accept_utp_peer",
                                result = "error",
                                error = %e,
                                "uTP accept failed"
                            );
                        }
                    }
                }
                _ = tier_tick.tick() => {
                    if self.config.runtime.torrent_tiers_enabled {
                        self.promote_due_tracker_torrents(Instant::now()).await;
                        self.reconcile_activity_tiers().await;
                    }
                }
            }
        }
        self.shutdown_torrent_tasks().await;
        self.storage_jobs
            .shutdown(Duration::from_secs(
                self.config.daemon.shutdown_timeout_secs.max(1),
            ))
            .await;
        if let Some(tx) = self.dht_tx.take() {
            shutdown_dht_task(
                tx,
                Duration::from_secs(self.config.daemon.shutdown_timeout_secs.max(1)),
            )
            .await;
        }
        self.append_session_event(
            None,
            EVENT_ENGINE_STOPPED,
            Some("native engine stopped"),
            serde_json::json!({}),
        );
        info!(
            component = "engine",
            operation = "shutdown",
            result = "ok",
            "engine shut down"
        );
        if let Some(reply) = self.shutdown_reply.take() {
            let _ = reply.send(());
        }
    }

    /// Returns false if the engine should stop.
    async fn handle_cmd(&mut self, cmd: EngineCmd) -> bool {
        match cmd {
            EngineCmd::Shutdown { reply } => {
                self.shutdown_reply = Some(reply);
                return false;
            }

            EngineCmd::AddTorrent {
                meta,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => {
                let result = self
                    .add_torrent(*meta, save_path, paused, category, tags)
                    .await;
                let _ = reply.send(result);
            }

            EngineCmd::AddMagnet {
                magnet,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => {
                let result = self
                    .add_magnet(magnet, save_path, paused, category, tags)
                    .await;
                let _ = reply.send(result);
            }

            EngineCmd::CompleteMagnet { info_hash, raw } => {
                if let Err(e) = self.complete_magnet(&info_hash, raw).await {
                    warn!(
                        component = "engine",
                        operation = "complete_magnet",
                        torrent = %info_hash,
                        result = "error",
                        error = %e,
                        "failed to complete magnet metadata"
                    );
                }
            }

            EngineCmd::IncomingPeer { info_hash, command } => {
                if let Err(e) = self.route_incoming_peer(info_hash.clone(), command).await {
                    warn!(
                        component = "peer_listener",
                        operation = "route_incoming_peer",
                        torrent = %info_hash,
                        result = "rejected",
                        error = %e,
                        "failed to promote or route inbound peer"
                    );
                }
            }

            EngineCmd::RemoveTorrent {
                info_hash,
                delete_files,
                reply,
            } => {
                if let Some(tx) = self.torrent_chans.remove(&info_hash) {
                    let _ = tx.send(TorrentCmd::Shutdown).await;
                }
                if let Some(mut task) = self.torrent_tasks.remove(&info_hash) {
                    match timeout(Duration::from_secs(10), &mut task).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            warn!(
                                component = "engine",
                                operation = "remove_torrent_task",
                                torrent = %info_hash,
                                result = "join_error",
                                error = %error,
                                "torrent task ended with a join error during removal"
                            );
                        }
                        Err(_) => {
                            warn!(
                                component = "engine",
                                operation = "remove_torrent_task",
                                torrent = %info_hash,
                                result = "timeout",
                                "torrent task did not stop during removal; aborting"
                            );
                            task.abort();
                        }
                    }
                }
                self.tier_controller.remove(&info_hash);
                self.tier_last_active.remove(&info_hash);
                let result = {
                    let entry = {
                        let mut reg = self.registry.write().await;
                        reg.remove(&info_hash).map_err(|e| e.to_string())
                    };
                    match entry {
                        Ok(entry) => {
                            let v2_only = self.is_pure_v2_torrent(&info_hash);
                            if delete_files {
                                if let Err(e) =
                                    self.delete_payload_files(&info_hash, &entry.save_path)
                                {
                                    warn!(
                                        component = "storage",
                                        operation = "delete_payload",
                                        torrent = %info_hash,
                                        result = "error",
                                        error = %e,
                                        "failed to delete torrent payload files"
                                    );
                                }
                            }
                            if let Err(e) = self.delete_persisted_torrent(&info_hash) {
                                warn!(
                                    component = "db",
                                    operation = "delete_torrent",
                                    torrent = %info_hash,
                                    result = "error",
                                    error = %e,
                                    "failed to delete persisted torrent"
                                );
                            }
                            if !v2_only {
                                self.unregister_dht_torrent(&info_hash).await;
                            }
                            self.append_session_event(
                                Some(&info_hash),
                                EVENT_TORRENT_REMOVED,
                                Some("torrent removed"),
                                serde_json::json!({
                                    "delete_files": delete_files,
                                    "v2_only": v2_only,
                                }),
                            );
                            Ok(())
                        }
                        Err(err) => Err(err),
                    }
                };
                let _ = reply.send(result);
            }

            EngineCmd::PauseTorrent { info_hash, reply } => {
                self.unregister_dht_torrent(&info_hash).await;
                let result = match self.send_to_torrent(&info_hash, TorrentCmd::Pause).await {
                    Ok(()) => Ok(()),
                    Err(_) if self.metadata_placeholder_row(&info_hash).is_some() => {
                        self.update_metadata_placeholder_state(&info_hash, TorrentState::Paused)
                    }
                    Err(_) if self.is_taskless_pure_v2_torrent(&info_hash) => {
                        self.set_registry_state(&info_hash, TorrentState::Paused, None)
                            .await
                    }
                    Err(_) => {
                        self.set_registry_state(&info_hash, TorrentState::Paused, None)
                            .await
                    }
                };
                if result.is_ok() {
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_TORRENT_PAUSED,
                        Some("torrent paused"),
                        serde_json::json!({}),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::ResumeTorrent { info_hash, reply } => {
                let v2_only_placeholder = self
                    .metadata_placeholder_row(&info_hash)
                    .as_ref()
                    .is_some_and(is_v2_only_placeholder_row);
                let taskless_v2 = self.is_taskless_pure_v2_torrent(&info_hash);
                let result = if taskless_v2 {
                    Err("pure v2 peer transfer is not implemented".to_owned())
                } else if v2_only_placeholder {
                    self.update_metadata_placeholder_state(
                        &info_hash,
                        TorrentState::MetadataPending,
                    )
                } else {
                    let result = self.ensure_torrent_task(&info_hash).await.and_then(|_| {
                        self.torrent_chans
                            .get(&info_hash)
                            .ok_or_else(|| format!("torrent {info_hash} not found"))
                            .map(|_| ())
                    });
                    if result.is_ok() {
                        self.send_to_torrent(&info_hash, TorrentCmd::Resume).await
                    } else {
                        result
                    }
                };
                if result.is_ok() {
                    if !v2_only_placeholder && !taskless_v2 {
                        self.register_dht_torrent_from_storage_or_hash(&info_hash)
                            .await;
                    }
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_TORRENT_RESUMED,
                        Some("torrent resumed"),
                        serde_json::json!({
                            "v2_only": v2_only_placeholder || taskless_v2,
                            "skipped": taskless_v2,
                        }),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::RecheckTorrent { info_hash, reply } => {
                let job_id = self.create_recheck_job(&info_hash);
                let result = if self.is_pure_v2_torrent(&info_hash) {
                    self.send_to_torrent(
                        &info_hash,
                        TorrentCmd::Recheck {
                            job_id: job_id.clone(),
                        },
                    )
                    .await
                } else {
                    match self.ensure_torrent_task(&info_hash).await {
                        Ok(()) => {
                            self.send_to_torrent(
                                &info_hash,
                                TorrentCmd::Recheck {
                                    job_id: job_id.clone(),
                                },
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    }
                };
                let result = if result.is_err() && self.is_pure_v2_torrent(&info_hash) {
                    self.recheck_pure_v2_torrent(&info_hash, job_id.clone())
                        .await
                } else {
                    result
                };
                if result.is_ok() {
                    if let Some(job_id) = &job_id {
                        self.update_job_state(
                            job_id,
                            JOB_STATE_RUNNING,
                            None,
                            Some("recheck dispatched to torrent task"),
                        );
                    }
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_RECHECK_REQUESTED,
                        Some("torrent recheck requested"),
                        serde_json::json!({ "job_id": job_id }),
                    );
                } else if let Some(job_id) = &job_id {
                    self.update_job_state(
                        job_id,
                        JOB_STATE_FAILED,
                        result.as_ref().err().cloned(),
                        Some("recheck dispatch failed"),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::PauseJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_PAUSED).await;
                let _ = reply.send(result);
            }

            EngineCmd::ResumeJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_RUNNING).await;
                let _ = reply.send(result);
            }

            EngineCmd::CancelJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_CANCELLED).await;
                let _ = reply.send(result);
            }

            EngineCmd::ReannounceTorrent { info_hash, reply } => {
                let v2_only_placeholder = self
                    .metadata_placeholder_row(&info_hash)
                    .as_ref()
                    .is_some_and(is_v2_only_placeholder_row);
                let taskless_v2 = self.is_taskless_pure_v2_torrent(&info_hash);
                let result = if v2_only_placeholder {
                    Ok(())
                } else if taskless_v2 {
                    Err("pure v2 tracker lifecycle is not implemented".to_owned())
                } else {
                    let was_taskless = !self.torrent_chans.contains_key(&info_hash);
                    match self.ensure_torrent_task(&info_hash).await {
                        Ok(()) => {
                            if was_taskless {
                                let _ = self.send_to_torrent(&info_hash, TorrentCmd::Resume).await;
                            }
                            self.send_to_torrent(&info_hash, TorrentCmd::Reannounce)
                                .await
                        }
                        Err(error) => Err(error),
                    }
                };
                if result.is_ok() {
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_REANNOUNCE_REQUESTED,
                        Some("tracker reannounce requested"),
                        serde_json::json!({
                            "v2_only": v2_only_placeholder || taskless_v2,
                            "skipped": v2_only_placeholder || taskless_v2,
                        }),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::GetTorrentMetadata { info_hash, reply } => {
                let result = self
                    .load_torrent_metadata(&info_hash)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            EngineCmd::GetTorrentBlob { info_hash, reply } => {
                let result = self
                    .load_torrent_blob(&info_hash)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            EngineCmd::GetTorrentTrackers { info_hash, reply } => {
                let result = self.torrent_trackers_inner(&info_hash);
                let _ = reply.send(result);
            }

            EngineCmd::UpdateTorrentLabels {
                info_hash,
                category,
                add_tags,
                remove_tags,
                reply,
            } => {
                let result = self
                    .update_torrent_labels_inner(&info_hash, category, add_tags, remove_tags)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentFields {
                info_hash,
                name,
                save_path,
                reply,
            } => {
                let result = self
                    .update_torrent_fields_inner(&info_hash, name, save_path)
                    .await
                    .map(|_| ());
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentFieldsWithJob {
                info_hash,
                name,
                save_path,
                reply,
            } => {
                let result = self
                    .update_torrent_fields_inner(&info_hash, name, save_path)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::ExecuteStoragePlan {
                operation,
                affected_torrents,
                plan,
                completed_steps,
                reply,
            } => {
                let quiesced = self
                    .quiesce_torrents_for_storage_plan(&affected_torrents)
                    .await;
                let (completion, completion_rx) = oneshot::channel();
                let result = self.queue_storage_plan_job(
                    &operation,
                    affected_torrents.clone(),
                    &plan,
                    completed_steps,
                    completion,
                );
                if let Ok(job_id) = &result {
                    let cmd_tx = self.cmd_tx.clone();
                    let job_id = job_id.clone();
                    tokio::spawn(async move {
                        let succeeded = completion_rx
                            .await
                            .map(|completion: StorageJobCompletion| completion.succeeded)
                            .unwrap_or(false);
                        let _ = cmd_tx
                            .send(EngineCmd::StoragePlanFinished {
                                job_id,
                                affected_torrents: quiesced,
                                succeeded,
                            })
                            .await;
                    });
                } else {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                }
                let _ = reply.send(result);
            }
            EngineCmd::StoragePlanFinished {
                job_id,
                affected_torrents,
                succeeded,
            } => {
                if !succeeded {
                    warn!(
                        component = "storage_jobs",
                        operation = "complete",
                        job_id = %job_id,
                        result = "failed",
                        "storage plan finished without a successful commit"
                    );
                }
                self.resume_torrents_after_storage_plan(affected_torrents)
                    .await;
            }
            EngineCmd::StorageMoveFinished {
                job_id,
                info_hash,
                name,
                old_save_path,
                save_path,
                quiesced,
                succeeded,
            } => {
                if let Err(error) = self
                    .finish_storage_move(
                        &job_id,
                        &info_hash,
                        name,
                        old_save_path,
                        save_path,
                        quiesced,
                        succeeded,
                    )
                    .await
                {
                    warn!(
                        component = "storage_jobs",
                        operation = "finish_storage_move",
                        job_id = %job_id,
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "failed to finalize asynchronous storage move"
                    );
                }
            }
            EngineCmd::ListJobs { reply } => {
                let result = self.list_active_jobs();
                let _ = reply.send(result);
            }
            EngineCmd::ListStorageRoots { reply } => {
                let result = self.list_storage_roots_inner();
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentTrackers {
                info_hash,
                trackers,
                reply,
            } => {
                let result = self
                    .update_torrent_trackers_inner(&info_hash, trackers)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::GetTorrentLimits { info_hash, reply } => {
                let result = self.torrent_limits_inner(&info_hash).await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentLimits {
                info_hash,
                limits,
                reply,
            } => {
                let result = self.update_torrent_limits_inner(&info_hash, limits).await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateFilePriorities {
                info_hash,
                file_ids,
                priority,
                reply,
            } => {
                let result = self
                    .update_file_priorities_inner(&info_hash, file_ids, priority)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::RenameFilePath {
                info_hash,
                file_id,
                new_path,
                reply,
            } => {
                let result = self
                    .rename_file_path_inner(&info_hash, file_id, new_path)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::RenameFolderPath {
                info_hash,
                old_path,
                new_path,
                reply,
            } => {
                let result = self
                    .rename_folder_path_inner(&info_hash, old_path, new_path)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::AddPeers {
                info_hash,
                peers,
                reply,
            } => {
                let result = self.add_peers_inner(&info_hash, peers).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetTorrentPeers { info_hash, reply } => {
                let result = self.torrent_peers_inner(&info_hash).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetTorrentWebseeds { info_hash, reply } => {
                let result = self.torrent_webseeds_inner(&info_hash).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetGlobalLimits { reply } => {
                let result = self.global_limits_inner();
                let _ = reply.send(result);
            }
            EngineCmd::UpdateGlobalLimits { limits, reply } => {
                let result = self.update_global_limits_inner(limits.clone());
                if result.is_ok() {
                    for tx in self.torrent_chans.values() {
                        let _ = tx
                            .send(TorrentCmd::UpdateGlobalLimits(limits.clone()))
                            .await;
                    }
                }
                let _ = reply.send(result);
            }
            EngineCmd::GetNetworkFeatures { reply } => {
                let result = self.network_features_inner();
                let _ = reply.send(result);
            }
            EngineCmd::UpdateNetworkFeatures { features, reply } => {
                let result = self.update_network_features_inner(features).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetQueuePriority { info_hash, reply } => {
                let result = self.queue_priority_inner(&info_hash);
                let _ = reply.send(result);
            }
            EngineCmd::UpdateQueueOrder {
                info_hashes,
                queue_move,
                reply,
            } => {
                let result = self.update_queue_order_inner(info_hashes, queue_move).await;
                let _ = reply.send(result);
            }

            EngineCmd::GetStats { reply } => {
                let result = self.engine_stats().await;
                let _ = reply.send(result);
            }

            EngineCmd::GetHealth { reply } => {
                let result = self.engine_subsystem_health().await;
                let _ = reply.send(result);
            }

            EngineCmd::ListSessionEvents {
                info_hash,
                kind,
                levels,
                last_known_id,
                limit,
                reply,
            } => {
                let result = self
                    .list_session_events(
                        info_hash.as_deref(),
                        kind.as_deref(),
                        &levels,
                        last_known_id,
                        limit,
                    )
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            EngineCmd::ReserveMemory {
                class,
                bytes,
                reply,
            } => {
                let _ = reply.send(Ok(self.resources.try_acquire(class, bytes)));
            }

            EngineCmd::DiagnoseTorrent { info_hash, reply } => {
                let result = self.diagnose_torrent_inner(&info_hash).await;
                let _ = reply.send(result);
            }
        }
        true
    }

    async fn add_torrent(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash_hex = meta_info_hash_hex(&meta);

        if self.torrent_chans.contains_key(&info_hash_hex) {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path(&save)?;

        // Register in session
        {
            let mut reg = self.registry.write().await;
            let mut entry = TorrentEntry::new(
                info_hash_hex.clone(),
                meta.name().to_owned(),
                save.to_string_lossy().into_owned(),
            );
            entry.total_length = meta_total_length(&meta);
            entry.amount_left = entry.total_length;
            entry.category = normalize_category(category);
            entry.tags = normalize_tags(tags);
            reg.add(entry).map_err(|e| e.to_string())?;
            // TorrentEntry starts in Stopped; transition to target state.
            let target = if paused || matches!(meta, TorrentMeta::V2(_)) {
                TorrentState::Paused
            } else {
                TorrentState::Downloading
            };
            if let Some(e) = reg.get_mut(&info_hash_hex) {
                let _ = e.transition(target);
            }
        }

        if let Err(error) = self.save_torrent_blob(&info_hash_hex, meta_raw(&meta)) {
            // TNG-008: `reg.add(entry)` above already made this torrent
            // visible to any concurrent reader (list/get). If we can't
            // even write its blob, don't leave that phantom row behind --
            // nothing could ever load this torrent's metadata again.
            let _ = self.registry.write().await.remove(&info_hash_hex);
            return Err(error.to_string());
        }
        let persisted = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(&info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing from registry"))?;
            self.persist_entry(entry, &meta)
        };
        if let Err(error) = persisted {
            // Same rollback, and also clean up the blob the previous step
            // wrote -- left alone it would be an orphan file with nothing
            // in the registry or DB pointing at it.
            let _ = self.registry.write().await.remove(&info_hash_hex);
            if let Err(cleanup_error) =
                std::fs::remove_file(torrent_blob_path(&self.config, &info_hash_hex))
            {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        component = "engine",
                        operation = "add_torrent_rollback",
                        torrent = %info_hash_hex,
                        error = %cleanup_error,
                        "failed to remove orphaned torrent blob after a failed add"
                    );
                }
            }
            return Err(error.to_string());
        }

        let is_private = meta.is_private();
        let torrent_name = meta.name().to_owned();
        if let Some(v1) = meta_v1(meta) {
            let info_hash = v1.info_hash;
            let _cmd_tx = self.spawn_torrent_task(info_hash_hex.clone(), v1, save, paused);
            if !paused && !is_private {
                self.register_dht_torrent(info_hash, &info_hash_hex).await;
            }
        }
        self.append_session_event(
            Some(&info_hash_hex),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({
                "paused": paused || !self.torrent_chans.contains_key(&info_hash_hex),
                "private": is_private,
                "name": torrent_name,
                "v2_only": !self.torrent_chans.contains_key(&info_hash_hex),
            }),
        );
        info!(
            component = "engine",
            operation = "add_torrent",
            torrent = %info_hash_hex,
            paused,
            result = "ok",
            "torrent added"
        );
        Ok(info_hash_hex)
    }

    async fn add_magnet(
        &mut self,
        magnet: MagnetLink,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash_hex = magnet
            .info_hash_v1
            .map(hex::encode)
            .or_else(|| magnet.info_hash_v2.map(hex::encode))
            .ok_or_else(|| "magnet is missing an info hash".to_owned())?;
        if self.torrent_chans.contains_key(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path(&save)?;
        let name = magnet
            .display_name
            .clone()
            .unwrap_or_else(|| info_hash_hex.clone());
        let mut entry = TorrentEntry::new(
            info_hash_hex.clone(),
            name,
            save.to_string_lossy().into_owned(),
        );
        entry.category = normalize_category(category);
        entry.tags = normalize_tags(tags);
        entry.state = if paused {
            TorrentState::Paused
        } else {
            TorrentState::MetadataPending
        };

        {
            let mut reg = self.registry.write().await;
            reg.add(entry.clone()).map_err(|e| e.to_string())?;
        }

        let row = TorrentRow {
            info_hash: entry.info_hash.clone(),
            name: entry.name.clone(),
            total_length: 0,
            piece_length: 0,
            piece_count: 0,
            is_private: false,
            save_path: entry.save_path.clone(),
            category: entry.category.clone(),
            tags: entry.tags.clone(),
            state: entry.state.as_str().to_owned(),
            added_at: entry.added_at as i64,
            completed_at: None,
            uploaded: 0,
            downloaded: 0,
            ratio: 0.0,
            trackers: magnet.trackers.clone(),
        };
        {
            let mut db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
            let tracker_rows = tracker_rows_from_urls(
                &entry.info_hash,
                &row.trackers,
                entry.stats.uploaded as i64,
                entry.stats.downloaded as i64,
                entry.total_length.saturating_sub(entry.stats.downloaded) as i64,
            );
            rt_db::replace_torrent_trackers(&mut db, &entry.info_hash, &tracker_rows)
                .map_err(|e| e.to_string())?;
        }

        if let Some(info_hash) = magnet.info_hash_v1 {
            let _cmd_tx = self.spawn_metadata_task(
                info_hash,
                info_hash_hex.clone(),
                magnet.trackers.clone(),
                paused,
            );
            if !paused {
                self.register_dht_torrent(info_hash, &info_hash_hex).await;
            }
        }
        self.append_session_event(
            Some(&info_hash_hex),
            EVENT_MAGNET_ADDED,
            Some("magnet added as metadata pending"),
            serde_json::json!({
                "paused": paused,
                "trackers": magnet.trackers,
                "v2_only": magnet.info_hash_v1.is_none(),
            }),
        );
        info!(
            component = "engine",
            operation = "add_magnet",
            torrent = %info_hash_hex,
            paused,
            result = "ok",
            "magnet added as metadata pending"
        );
        Ok(info_hash_hex)
    }

    async fn complete_magnet(&mut self, info_hash_hex: &str, raw: Vec<u8>) -> CmdResult<()> {
        let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
        let fetched_hash = meta_info_hash_hex(&meta);
        if fetched_hash != info_hash_hex {
            return Err(format!(
                "fetched metadata hash {fetched_hash} does not match magnet {info_hash_hex}"
            ));
        }
        let is_private = meta.is_private();
        let torrent_name = meta.name().to_owned();
        let total_length = meta_total_length(&meta);
        let v2_only = matches!(meta, TorrentMeta::V2(_));

        let (save, category, tags) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            (
                PathBuf::from(&entry.save_path),
                entry.category.clone(),
                entry.tags.clone(),
            )
        };
        self.authorize_storage_path(&save)?;

        self.save_torrent_blob(info_hash_hex, &raw)
            .map_err(|e| e.to_string())?;
        {
            let mut reg = self.registry.write().await;
            let entry = reg
                .get_mut(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            entry.name = torrent_name.clone();
            entry.total_length = total_length;
            entry.amount_left = total_length;
            entry.category = category;
            entry.tags = tags;
            if v2_only {
                let _ = entry.transition(TorrentState::Paused);
            } else {
                let _ = entry.transition(TorrentState::Downloading);
            }
        }
        {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing after metadata update"))?;
            self.persist_entry(entry, &meta)
                .map_err(|e| e.to_string())?;
        }

        if let Some(old_tx) = self.torrent_chans.remove(info_hash_hex) {
            let _ = old_tx.send(TorrentCmd::Shutdown).await;
        }
        if let Some(old_task) = self.torrent_tasks.remove(info_hash_hex) {
            tokio::spawn(async move {
                let _ = timeout(Duration::from_secs(10), old_task).await;
            });
        }
        if let Some(v1) = meta_v1(meta) {
            let info_hash = v1.info_hash;
            let _tx = self.spawn_torrent_task(info_hash_hex.to_owned(), v1, save, false);
            if !is_private {
                self.register_dht_torrent(info_hash, info_hash_hex).await;
            }
        }
        self.append_session_event(
            Some(info_hash_hex),
            EVENT_METADATA_RESOLVED,
            Some("magnet metadata resolved"),
            serde_json::json!({
                "name": torrent_name,
                "total_length": total_length,
                "private": is_private,
                "v2_only": v2_only,
            }),
        );
        info!(
            component = "engine",
            operation = "complete_magnet",
            torrent = %info_hash_hex,
            result = "ok",
            "magnet metadata completed"
        );
        Ok(())
    }

    fn spawn_torrent_task(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMetaV1,
        save: PathBuf,
        paused: bool,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let task = TorrentTask::new(
            meta,
            save,
            paused,
            Arc::clone(&self.registry),
            Arc::clone(&self.db),
            self.resources.clone(),
            cmd_rx,
            fastresume_dir(&self.config),
            self.config.network.max_peers,
            self.config.network.listen_port,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            self.config.tracker.min_interval_secs,
            self.config
                .memory
                .piece_assembly_cap_mb
                .saturating_mul(1024 * 1024) as usize,
            storage_io_config_from_config(&self.config),
            self.peer_exchange_enabled(),
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.network_budget.clone(),
        );
        let handle = tokio::spawn(task.run());
        let tier_key = info_hash_hex.clone();
        self.torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.torrent_tasks.insert(info_hash_hex, handle);
        self.tier_last_active.insert(tier_key, Instant::now());
        cmd_tx
    }

    fn spawn_metadata_task(
        &mut self,
        info_hash: [u8; 20],
        info_hash_hex: String,
        trackers: Vec<String>,
        paused: bool,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let handle = tokio::spawn(run_metadata_task(
            info_hash,
            info_hash_hex.clone(),
            trackers,
            cmd_rx,
            self.cmd_tx.clone(),
            self.resources.clone(),
            self.config.network.listen_port,
            self.config.network.max_peers,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            paused,
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.network_budget.clone(),
        ));
        let tier_key = info_hash_hex.clone();
        self.torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.torrent_tasks.insert(info_hash_hex, handle);
        self.tier_last_active.insert(tier_key, Instant::now());
        cmd_tx
    }

    async fn shutdown_torrent_tasks(&mut self) {
        let task_count = self.torrent_chans.len();
        for tx in self.torrent_chans.values() {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
        self.torrent_chans.clear();

        let timeout_secs = self.config.daemon.shutdown_timeout_secs.max(1);
        let timeout_budget = Duration::from_secs(timeout_secs);
        let deadline = Instant::now() + timeout_budget;
        let mut timed_out = false;

        for (info_hash, mut task) in std::mem::take(&mut self.torrent_tasks) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                timed_out = true;
                task.abort();
                warn!(
                    component = "engine",
                    operation = "shutdown_torrent_task",
                    torrent = %info_hash,
                    timeout_secs,
                    result = "timeout",
                    "aborted torrent task after shutdown deadline"
                );
                continue;
            };

            match timeout(remaining, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if !e.is_cancelled() {
                        warn!(
                            component = "engine",
                            operation = "shutdown_torrent_task",
                            torrent = %info_hash,
                            result = "error",
                            error = %e,
                            "torrent task failed during shutdown"
                        );
                    }
                }
                Err(_) => {
                    timed_out = true;
                    task.abort();
                    warn!(
                        component = "engine",
                        operation = "shutdown_torrent_task",
                        torrent = %info_hash,
                        timeout_secs,
                        result = "timeout",
                        "aborted torrent task after shutdown deadline"
                    );
                }
            }
        }

        if !timed_out {
            info!(
                component = "engine",
                operation = "shutdown_torrent_tasks",
                tasks = task_count,
                result = "ok",
                "torrent tasks stopped cleanly"
            );
        }
    }

    /// Promote dormant torrents whose persisted tracker deadlines are due.
    /// Dormant torrents are represented by the registry/SQLite/blob and do
    /// not participate in a periodic per-torrent async loop. A seed that has
    /// been idle through the
    /// warm window is demoted and reconstructed on the next lifecycle, peer,
    /// or persisted tracker-deadline demand.
    async fn promote_due_tracker_torrents(&mut self, now: Instant) {
        let due = self.tier_controller.pop_due_tracker_checks(now);
        if due.is_empty() {
            return;
        }
        let now_unix = unix_now_i64();
        for info_hash in due {
            if self.torrent_chans.contains_key(&info_hash) {
                continue;
            }
            let state = {
                let registry = self.registry.read().await;
                registry.get(&info_hash).map(|entry| entry.state)
            };
            if state != Some(TorrentState::Seeding) {
                continue;
            }
            let Some(deadline) = self.persisted_tracker_deadline(&info_hash) else {
                continue;
            };
            if deadline > now_unix {
                if let Some(deadline) = unix_deadline_to_instant(deadline, now_unix, now) {
                    self.tier_controller
                        .schedule_tracker_check(info_hash, deadline);
                }
                continue;
            }

            match self.ensure_torrent_task(&info_hash).await {
                Ok(()) => {
                    let Some(tx) = self.torrent_chans.get(&info_hash).cloned() else {
                        continue;
                    };
                    if tx.send(TorrentCmd::Resume).await.is_err()
                        || tx.send(TorrentCmd::Reannounce).await.is_err()
                    {
                        warn!(
                            component = "tiering",
                            operation = "promote_tracker_due",
                            torrent = %info_hash,
                            result = "error",
                            "promoted tracker-due torrent but its task stopped before reannounce"
                        );
                    } else {
                        self.tier_last_active.insert(info_hash.clone(), now);
                        info!(
                            component = "tiering",
                            operation = "promote_tracker_due",
                            torrent = %info_hash,
                            result = "ok",
                            "promoted dormant torrent for persisted tracker deadline"
                        );
                    }
                }
                Err(error) => {
                    warn!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "failed to promote dormant torrent for tracker deadline"
                    );
                    self.tier_controller
                        .schedule_tracker_check(info_hash, now + Duration::from_secs(30));
                }
            }
        }
    }

    /// Return the earliest persisted announce deadline for a torrent without
    /// retaining a per-torrent database object in the engine actor.
    fn persisted_tracker_deadline(&self, info_hash: &str) -> Option<i64> {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_torrent_trackers(&db, info_hash)
            .ok()?
            .into_iter()
            .filter_map(|tracker| tracker.next_announce_at)
            .min()
    }

    fn schedule_persisted_tracker_deadline(&mut self, info_hash: &str, now: Instant) {
        let Some(deadline) = self.persisted_tracker_deadline(info_hash) else {
            return;
        };
        let now_unix = unix_now_i64();
        if let Some(deadline) = unix_deadline_to_instant(deadline, now_unix, now) {
            self.tier_controller
                .schedule_tracker_check(info_hash.to_owned(), deadline);
        }
    }

    /// Reconcile only the currently promoted task set. Dormant torrents are
    /// represented by the registry/SQLite/blob and do not participate in a
    /// periodic per-torrent async loop. A seed that has been idle through the
    /// warm window is demoted and reconstructed on the next lifecycle, peer,
    /// or persisted tracker-deadline demand.
    async fn reconcile_activity_tiers(&mut self) {
        if !self.config.runtime.torrent_tiers_enabled {
            return;
        }
        let task_ids = self.torrent_chans.keys().cloned().collect::<Vec<_>>();
        let states = {
            let registry = self.registry.read().await;
            task_ids
                .iter()
                .filter_map(|info_hash| {
                    registry
                        .get(info_hash)
                        .map(|entry| (info_hash.clone(), entry.state))
                })
                .collect::<HashMap<_, _>>()
        };
        let now = Instant::now();
        let mut demote = Vec::new();
        for info_hash in task_ids {
            let Some(state) = states.get(&info_hash).copied() else {
                continue;
            };
            let connected_peers = self.torrent_chans.get(&info_hash).and_then(|tx| {
                let (reply, rx) = oneshot::channel();
                tx.try_send(TorrentCmd::GetRuntimeStats { reply })
                    .ok()
                    .map(|_| rx)
            });
            let runtime = match connected_peers {
                Some(rx) => timeout(Duration::from_millis(50), rx)
                    .await
                    .ok()
                    .and_then(Result::ok),
                None => None,
            };
            let connected = runtime
                .as_ref()
                .map(|stats| stats.connected_peers as usize)
                .unwrap_or_default();
            let outstanding = runtime
                .as_ref()
                .map(|stats| stats.outstanding_requests as usize)
                .unwrap_or_default();
            if connected > 0 || outstanding > 0 {
                self.tier_last_active.insert(info_hash.clone(), now);
            }
            let decision = self.tier_controller.apply_input(
                info_hash.clone(),
                TierInput {
                    state,
                    connected_peers: connected,
                    outstanding_requests: outstanding,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: self.tier_last_active.get(&info_hash).copied(),
                    now,
                },
            );
            if decision.tier == crate::tier::TorrentActivityTier::Dormant
                && state == TorrentState::Seeding
            {
                demote.push(info_hash);
            }
        }
        for info_hash in demote {
            self.demote_torrent_task(&info_hash).await;
        }
    }

    async fn demote_torrent_task(&mut self, info_hash: &str) {
        let Some(tx) = self.torrent_chans.remove(info_hash) else {
            return;
        };
        self.unregister_dht_torrent(info_hash).await;
        let _ = tx.send(TorrentCmd::Shutdown).await;
        if let Some(mut task) = self.torrent_tasks.remove(info_hash) {
            let timeout_budget = Duration::from_secs(10);
            if timeout(timeout_budget, &mut task).await.is_err() {
                task.abort();
            }
        }
        // Keep the dormant key in the controller. Removing it made the
        // controller's tracked set shrink on every demotion, so the runtime
        // could no longer account for all 100k rows even though the registry
        // still retained them. The controller entry is only a compact tier
        // state/timer record; actual torrent removal still calls `remove`.
        let state = self
            .registry
            .read()
            .await
            .get(info_hash)
            .map(|entry| entry.state)
            .unwrap_or(TorrentState::Seeding);
        self.tier_controller.apply_input(
            info_hash.to_owned(),
            TierInput {
                state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: None,
                now: Instant::now(),
            },
        );
        let now = Instant::now();
        self.schedule_persisted_tracker_deadline(info_hash, now);
        let tracker_deadline = self
            .persisted_tracker_deadline(info_hash)
            .and_then(|deadline| unix_deadline_to_instant(deadline, unix_now_i64(), now));
        let piece_count = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash)
                .ok()
                .map(|row| row.piece_count.max(0).min(u32::MAX as i64) as u32)
                .unwrap_or_default()
        };
        self.tier_controller.set_dormant_snapshot(
            info_hash.to_owned(),
            dormant_snapshot_from_fields(info_hash, state, piece_count, tracker_deadline),
        );
        self.tier_last_active.remove(info_hash);
        info!(
            component = "tiering",
            operation = "demote",
            torrent = %info_hash,
            result = "ok",
            "demoted idle torrent to dormant representation"
        );
    }

    async fn route_incoming_peer(
        &mut self,
        info_hash: String,
        command: TorrentCmd,
    ) -> CmdResult<()> {
        let was_taskless = !self.torrent_chans.contains_key(&info_hash);
        if was_taskless {
            self.ensure_torrent_task(&info_hash).await?;
        }
        let tx = self
            .torrent_chans
            .get(&info_hash)
            .cloned()
            .ok_or_else(|| format!("torrent {info_hash} has no runtime task"))?;
        if was_taskless {
            tx.send(TorrentCmd::Resume)
                .await
                .map_err(|_| "promoted torrent task stopped before resume".to_owned())?;
        }
        tx.send(command)
            .await
            .map_err(|_| "promoted torrent task stopped before peer delivery".to_owned())?;

        let now = Instant::now();
        self.tier_last_active.insert(info_hash.clone(), now);
        let state = self
            .registry
            .read()
            .await
            .get(&info_hash)
            .map(|entry| entry.state)
            .unwrap_or(TorrentState::Downloading);
        self.tier_controller.apply_event(
            info_hash,
            TierInput {
                state,
                connected_peers: 1,
                outstanding_requests: 0,
                inbound_peer: true,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
            TierEvent::InboundPeer,
        );
        Ok(())
    }

    async fn load_persisted_torrents(&mut self) -> anyhow::Result<()> {
        let rows = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_all(&db)?
        };
        // Resolve storage authority once for the entire restore. The old
        // path rebuilt/canonicalized the configured roots for every row,
        // turning a large restore into an avoidable O(torrents * roots)
        // filesystem/SQLite loop.
        let storage_authority = self
            .configured_storage_authority()
            .map_err(anyhow::Error::msg)?;
        self.repair_missing_torrent_tracker_rows(&rows)?;
        let tracker_deadlines = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_all_torrent_trackers(&db)?
                .into_iter()
                .filter_map(|tracker| {
                    tracker
                        .next_announce_at
                        .map(|deadline| (tracker.info_hash, deadline))
                })
                .fold(
                    HashMap::<String, i64>::new(),
                    |mut deadlines, (hash, deadline)| {
                        deadlines
                            .entry(hash)
                            .and_modify(|current| *current = (*current).min(deadline))
                            .or_insert(deadline);
                        deadlines
                    },
                )
        };
        let restore_now = Instant::now();
        let restore_now_unix = unix_now_i64();
        let mut authorized_save_paths = HashMap::<String, Result<(), String>>::new();
        let mut dormant_restored = 0_u64;

        for row in rows {
            let authorization = authorized_save_paths
                .entry(row.save_path.clone())
                .or_insert_with(|| {
                    storage_authority
                        .authorize_path(Path::new(&row.save_path))
                        .map_err(|error| error.to_string())
                })
                .clone();
            if let Err(error) = authorization {
                warn!(
                    component = "storage",
                    operation = "restore_torrent",
                    torrent = %row.info_hash,
                    save_path = %row.save_path,
                    result = "rejected",
                    error = %error,
                    "skipping persisted torrent outside configured storage roots"
                );
                continue;
            }
            let state = state_from_str(&row.state);
            let start_task =
                !self.config.runtime.torrent_tiers_enabled || should_start_task_on_restore(state);
            if self.is_metadata_placeholder_row(&row) {
                let entry = entry_from_row(&row);
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(
                        component = "engine",
                        operation = "restore_metadata_pending_registry",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to restore metadata-pending registry entry"
                    );
                }
                drop(reg);
                self.tier_controller.apply_input(
                    row.info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: start_task.then_some(Instant::now()),
                        now: Instant::now(),
                    },
                );
                let mut restored = false;
                if let Ok(info_hash) = parse_info_hash_hex(&row.info_hash) {
                    if start_task {
                        let _tx = self.spawn_metadata_task(
                            info_hash,
                            row.info_hash.clone(),
                            row.trackers.clone(),
                            matches!(state, TorrentState::Paused | TorrentState::Stopped),
                        );
                        self.register_dht_torrent(info_hash, &row.info_hash).await;
                    }
                    self.append_session_event(
                        Some(&row.info_hash),
                        EVENT_TORRENT_RESTORED,
                        Some("metadata-pending torrent restored"),
                        serde_json::json!({
                            "state": row.state,
                            "metadata_pending": true,
                            "v2_only": false,
                        }),
                    );
                    restored = true;
                }
                if !restored {
                    self.append_session_event(
                        Some(&row.info_hash),
                        EVENT_TORRENT_RESTORED,
                        Some("metadata-pending torrent restored"),
                        serde_json::json!({
                            "state": row.state,
                            "metadata_pending": true,
                            "v2_only": row.info_hash.len() == 64,
                        }),
                    );
                }
                continue;
            }
            // A dormant row only needs its durable projection. Do not read or
            // parse the metainfo blob until a lifecycle command, peer, or
            // tracker deadline promotes it. This is the key restart property
            // for the 100k target: cold rows remain O(1) registry state and
            // do not allocate a task, scheduler, or parsed metainfo tree.
            if self.config.runtime.torrent_tiers_enabled && !start_task {
                let entry = entry_from_row(&row);
                {
                    let mut reg = self.registry.write().await;
                    if let Err(e) = reg.add(entry) {
                        warn!(
                            component = "engine",
                            operation = "restore_dormant_registry",
                            torrent = %row.info_hash,
                            result = "error",
                            error = %e,
                            "failed to restore dormant registry entry"
                        );
                        continue;
                    }
                }
                self.tier_controller.apply_input(
                    row.info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: None,
                        now: restore_now,
                    },
                );
                if state == TorrentState::Seeding {
                    if let Some(deadline) =
                        tracker_deadlines.get(&row.info_hash).and_then(|deadline| {
                            unix_deadline_to_instant(*deadline, restore_now_unix, restore_now)
                        })
                    {
                        self.tier_controller
                            .schedule_tracker_check(row.info_hash.clone(), deadline);
                    }
                }
                let tracker_deadline = tracker_deadlines.get(&row.info_hash).and_then(|deadline| {
                    unix_deadline_to_instant(*deadline, restore_now_unix, restore_now)
                });
                self.tier_controller.set_dormant_snapshot(
                    row.info_hash.clone(),
                    dormant_snapshot_from_row(&row, state, tracker_deadline),
                );
                dormant_restored = dormant_restored.saturating_add(1);
                continue;
            }
            let blob_path = torrent_blob_path(&self.config, &row.info_hash);
            let raw = match std::fs::read(&blob_path) {
                Ok(raw) => raw,
                Err(e) => {
                    warn!(
                        component = "engine",
                        operation = "load_persisted_torrent_metadata",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "persisted torrent metadata missing"
                    );
                    continue;
                }
            };
            let meta = match parse_torrent(&raw) {
                Ok(meta) => meta,
                Err(e) => {
                    warn!(
                        component = "engine",
                        operation = "parse_persisted_torrent",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to parse persisted torrent"
                    );
                    continue;
                }
            };
            let info_hash_hex = meta_info_hash_hex(&meta);
            if info_hash_hex != row.info_hash {
                warn!(
                    component = "engine",
                    operation = "load_persisted_torrent",
                    row_hash = %row.info_hash,
                    meta_hash = %info_hash_hex,
                    result = "error",
                    "persisted torrent hash mismatch"
                );
                continue;
            }
            let entry = entry_from_row(&row);
            {
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(
                        component = "engine",
                        operation = "restore_registry_entry",
                        torrent = %row.info_hash,
                        result = "error",
                        error = %e,
                        "failed to restore registry entry"
                    );
                    continue;
                }
            }

            self.tier_controller.apply_input(
                row.info_hash.clone(),
                TierInput {
                    state,
                    connected_peers: 0,
                    outstanding_requests: 0,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: start_task.then_some(Instant::now()),
                    now: Instant::now(),
                },
            );
            let is_private = meta.is_private();
            let v2_only = matches!(meta, TorrentMeta::V2(_));
            if start_task {
                if let Some(v1) = meta_v1(meta) {
                    let info_hash = v1.info_hash;
                    let _tx = self.spawn_torrent_task(
                        row.info_hash.clone(),
                        v1,
                        PathBuf::from(&row.save_path),
                        matches!(state, TorrentState::Paused | TorrentState::Stopped),
                    );
                    if !is_private {
                        self.register_dht_torrent(info_hash, &row.info_hash).await;
                    }
                }
            }
            self.append_session_event(
                Some(&row.info_hash),
                EVENT_TORRENT_RESTORED,
                Some("torrent restored from database"),
                serde_json::json!({
                    "state": row.state,
                    "private": is_private,
                    "v2_only": v2_only,
                }),
            );
            info!(
                component = "engine",
                operation = "restore_torrent",
                torrent = %row.info_hash,
                state = %row.state,
                task_started = start_task,
                result = "ok",
                "restored persisted torrent"
            );
        }
        if dormant_restored > 0 {
            // One aggregate event keeps a 100k restart from generating a
            // matching 100k-row event-log write burst. Per-torrent detail is
            // already available in the registry and durable torrent rows.
            self.append_session_event(
                None,
                EVENT_TORRENT_RESTORED,
                Some("dormant torrents restored from database"),
                serde_json::json!({
                    "count": dormant_restored,
                    "task_started": false,
                    "dormant": true,
                }),
            );
        }
        Ok(())
    }

    fn recover_interrupted_jobs(&self) -> anyhow::Result<()> {
        let now = unix_now_i64();
        let db = self.db.lock().expect("database mutex poisoned");
        let jobs = rt_db::list_active_jobs(&db)?;
        for mut job in jobs {
            let previous_state = job.state.clone();
            let recovered_state = match previous_state.as_str() {
                JOB_STATE_RUNNING | "cancelling" if job.kind == JOB_KIND_STORAGE_PLAN => {
                    Some(JOB_STATE_QUEUED)
                }
                JOB_STATE_RUNNING | "cancelling" => Some(JOB_STATE_PAUSED),
                _ => None,
            };
            let Some(recovered_state) = recovered_state else {
                continue;
            };
            job.state = recovered_state.to_owned();
            job.updated_at = now;
            rt_db::upsert_job(&db, &job)?;
            rt_db::append_job_event(
                &db,
                &rt_db::JobEventRow {
                    event_id: None,
                    job_id: job.job_id.clone(),
                    occurred_at: now,
                    kind: "job_recovered".to_owned(),
                    message: Some("job recovered after engine restart".to_owned()),
                    payload: serde_json::json!({
                        "state": recovered_state,
                        "previous_state": previous_state,
                    })
                    .to_string(),
                },
            )?;
        }
        Ok(())
    }

    /// Reconstruct storage plans from their durable queue/checkpoint event and
    /// hand them back to the bounded worker supervisor after restart. The
    /// worker owns the filesystem transaction; the actor only restores
    /// quiesce/resume and save-path finalization callbacks.
    async fn resume_recovered_storage_jobs(&self) -> anyhow::Result<()> {
        let jobs = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_active_jobs(&db)?
        };
        for job in jobs.into_iter().filter(|job| {
            job.kind == JOB_KIND_STORAGE_PLAN
                && matches!(job.state.as_str(), JOB_STATE_QUEUED | JOB_STATE_PAUSED)
        }) {
            let recovery = {
                let db = self.db.lock().expect("database mutex poisoned");
                rt_db::list_job_events(&db, &job.job_id, 64)?
                    .into_iter()
                    .find_map(|event| decode_storage_plan_event(&event.payload))
            };
            let Some((operation, plan, event_completed_steps, context)) = recovery else {
                self.update_job_state(
                    &job.job_id,
                    JOB_STATE_FAILED,
                    Some("storage plan payload is missing or invalid".to_owned()),
                    Some("storage plan recovery failed"),
                );
                continue;
            };
            // `job.checkpoint` stores a count for the legacy job projection,
            // not the step indexes themselves. Prefer the serialized event:
            // completed steps may be a sparse subset (for example `[2]`),
            // and turning that into `0..1` would silently skip the wrong
            // filesystem operation after restart. The count is only a
            // compatibility fallback for a crash between the job-row update
            // and its checkpoint-event insert.
            let checkpoint_steps =
                recovered_storage_plan_steps(&plan, job.checkpoint, event_completed_steps)
                    .map_err(anyhow::Error::msg)?;
            let quiesced = self
                .quiesce_torrents_for_storage_plan(&job.affected_torrents)
                .await;
            let roots = match self.configured_storage_roots_for_execution() {
                Ok(roots) => roots,
                Err(error) => {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                    self.update_job_state(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(error),
                        Some("storage plan recovery could not resolve roots"),
                    );
                    continue;
                }
            };
            let checkpoint_steps = match rt_storage::reconcile_storage_plan_under_roots(
                &plan,
                &roots,
                &checkpoint_steps,
            ) {
                Ok(steps) => steps,
                Err(error) => {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                    self.update_job_state(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(format!(
                            "storage plan filesystem reconciliation failed: {error}"
                        )),
                        Some("storage plan recovery found ambiguous filesystem state"),
                    );
                    continue;
                }
            };
            let (completion, completion_rx) = oneshot::channel();
            let submit_result = if job.state == JOB_STATE_PAUSED {
                self.storage_jobs.submit_paused(
                    Arc::clone(&self.db),
                    job.job_id.clone(),
                    operation.clone(),
                    plan,
                    checkpoint_steps,
                    roots,
                    completion,
                )
            } else {
                self.storage_jobs.submit(
                    Arc::clone(&self.db),
                    job.job_id.clone(),
                    operation.clone(),
                    plan,
                    checkpoint_steps,
                    roots,
                    completion,
                )
            };
            if let Err(error) = submit_result {
                self.resume_torrents_after_storage_plan(quiesced).await;
                self.update_job_state(
                    &job.job_id,
                    JOB_STATE_FAILED,
                    Some(error),
                    Some("storage plan recovery could not be queued"),
                );
                continue;
            }

            let cmd_tx = self.cmd_tx.clone();
            let job_id = job.job_id.clone();
            let affected_torrents = quiesced;
            let move_context = if operation == "move" {
                (|| {
                    let info_hash = job.affected_torrents.first()?.clone();
                    let old_save_path = context
                        .get("old_save_path")
                        .and_then(serde_json::Value::as_str)?
                        .into();
                    let save_path = context
                        .get("save_path")
                        .and_then(serde_json::Value::as_str)?
                        .into();
                    let name = context
                        .get("name")
                        .and_then(|value| value.as_str().map(ToOwned::to_owned));
                    Some((info_hash, name, old_save_path, save_path))
                })()
            } else {
                None
            };
            tokio::spawn(async move {
                let succeeded = completion_rx
                    .await
                    .map(|completion: StorageJobCompletion| completion.succeeded)
                    .unwrap_or(false);
                if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                    let quiesced = affected_torrents
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, paused)| *paused);
                    let _ = cmd_tx
                        .send(EngineCmd::StorageMoveFinished {
                            job_id,
                            info_hash,
                            name,
                            old_save_path,
                            save_path,
                            quiesced,
                            succeeded,
                        })
                        .await;
                } else {
                    let _ = cmd_tx
                        .send(EngineCmd::StoragePlanFinished {
                            job_id,
                            affected_torrents,
                            succeeded,
                        })
                        .await;
                }
            });
        }
        Ok(())
    }

    fn persist_entry(&self, entry: &TorrentEntry, meta: &TorrentMeta) -> anyhow::Result<()> {
        let row = row_from_entry(entry, meta);
        let mut db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row)?;
        persist_torrent_files(&mut db, &entry.info_hash, meta)?;
        sync_torrent_trackers_if_urls_changed(&mut db, &row)?;
        Ok(())
    }

    fn save_torrent_blob(&self, info_hash: &str, raw: &[u8]) -> anyhow::Result<()> {
        let path = torrent_blob_path(&self.config, info_hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, raw)?;
        Ok(())
    }

    fn load_torrent_blob(&self, info_hash: &str) -> anyhow::Result<Vec<u8>> {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        std::fs::read(&blob_path)
            .with_context(|| format!("reading persisted torrent blob {}", blob_path.display()))
    }

    fn delete_persisted_torrent(&self, info_hash: &str) -> anyhow::Result<()> {
        {
            let db = self.db.lock().expect("database mutex poisoned");
            let _ = rt_db::delete(&db, info_hash)?;
        }
        match std::fs::remove_file(torrent_blob_path(&self.config, info_hash)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        FastresumeStore::new(fastresume_dir(&self.config)).delete(info_hash)?;
        Ok(())
    }

    fn delete_payload_files(&self, info_hash: &str, save_path: &str) -> anyhow::Result<()> {
        self.authorize_storage_path(Path::new(save_path))
            .map_err(|error| anyhow::anyhow!(error))?;
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let raw = std::fs::read(&blob_path).with_context(|| {
            format!("reading persisted torrent metadata {}", blob_path.display())
        })?;
        let meta = parse_torrent(&raw)?;
        let root = PathBuf::from(save_path);
        for rel_path in meta_file_paths(&meta) {
            let path = rel_path.resolve(&root);
            self.authorize_storage_path(&path)
                .map_err(|error| anyhow::anyhow!(error))?;
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    prune_empty_dirs(path.parent(), &root)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn load_torrent_metadata(&self, info_hash: &str) -> anyhow::Result<EngineTorrentMetadata> {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let raw = match std::fs::read(&blob_path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let row = {
                    let db = self.db.lock().expect("database mutex poisoned");
                    rt_db::get(&db, info_hash)?
                };
                if self.is_metadata_placeholder_row(&row) {
                    return Ok(metadata_from_placeholder_row(&row));
                }
                return Err(e).with_context(|| {
                    format!("reading persisted torrent metadata {}", blob_path.display())
                });
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("reading persisted torrent metadata {}", blob_path.display())
                });
            }
        };
        let meta = parse_torrent(&raw)?;
        let mut metadata = metadata_from_meta(&meta);
        {
            let db = self.db.lock().expect("database mutex poisoned");
            if let Ok(files) = rt_db::list_torrent_files(&db, info_hash) {
                if !files.is_empty() {
                    let policy = files
                        .into_iter()
                        .map(|file| {
                            (
                                file.file_index as u32,
                                (file.path, file.priority, file.wanted),
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    for file in &mut metadata.files {
                        if let Some((path, priority, wanted)) = policy.get(&file.index) {
                            file.path = path.clone();
                            file.priority = *priority;
                            file.wanted = *wanted;
                        }
                    }
                }
            }
        }
        if let Ok(row) = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash)
        } {
            if !row.trackers.is_empty() {
                metadata.trackers = row.trackers;
            }
        }
        if let Ok(hash) = decode_info_hash_bytes(info_hash) {
            if let Ok(state) = FastresumeStore::new(fastresume_dir(&self.config)).load(info_hash) {
                if state.validate(&hash, metadata.piece_count as u32).is_ok() {
                    let mut pieces = state
                        .pieces
                        .iter()
                        .map(|piece| match piece {
                            PieceState::Valid => EnginePieceState::Complete,
                            PieceState::Invalid | PieceState::Unknown | PieceState::Missing => {
                                EnginePieceState::Missing
                            }
                        })
                        .collect::<Vec<_>>();
                    for partial in state.partial_pieces {
                        if let Some(piece) = pieces.get_mut(partial.piece as usize) {
                            if !partial.received_blocks.is_empty() {
                                *piece = EnginePieceState::Partial;
                            }
                        }
                    }
                    metadata.piece_states = pieces;
                }
            }
        }
        Ok(metadata)
    }

    fn is_pure_v2_torrent(&self, info_hash: &str) -> bool {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let Ok(raw) = std::fs::read(blob_path) else {
            return false;
        };
        matches!(parse_torrent(&raw), Ok(TorrentMeta::V2(_)))
    }

    fn is_taskless_pure_v2_torrent(&self, info_hash: &str) -> bool {
        !self.torrent_chans.contains_key(info_hash) && self.is_pure_v2_torrent(info_hash)
    }

    async fn recheck_pure_v2_torrent(
        &self,
        info_hash: &str,
        job_id: Option<String>,
    ) -> CmdResult<()> {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let raw = std::fs::read(&blob_path).map_err(|e| e.to_string())?;
        let meta = match parse_torrent(&raw).map_err(|e| e.to_string())? {
            TorrentMeta::V2(meta) => meta,
            _ => return Err(format!("torrent {info_hash} has no active torrent task")),
        };
        let save_root = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            PathBuf::from(&entry.save_path)
        };
        self.authorize_storage_path(&save_root)?;
        self.set_registry_state(info_hash, TorrentState::Checking, None)
            .await?;
        if let Some(job_id) = &job_id {
            self.update_job_state(
                job_id,
                JOB_STATE_RUNNING,
                None,
                Some("pure v2 recheck started"),
            );
        }

        let files = meta
            .files
            .iter()
            .map(|file| V2FileHash {
                file_index: file.index,
                path: file.path.clone(),
                length: file.length,
                pieces_root: file.pieces_root,
            })
            .collect::<Vec<_>>();
        let scheduler = MountScheduler::new_for_path(
            StorageRootId::new(),
            &save_root,
            &SchedulerConfig {
                profile: StorageProfile::Unknown,
                resources: Some(self.resources.clone()),
                storage_io: storage_io_config_from_config(&self.config),
                ..Default::default()
            },
        );
        let results = V2FileVerifier::new(&save_root, &scheduler, &files)
            .verify_all()
            .await;
        let invalid_files = results
            .iter()
            .filter_map(|(file_index, result)| {
                (!matches!(result, VerifyResult::Valid)).then_some(*file_index as i64)
            })
            .collect::<Vec<_>>();
        if let Some(job_id) = &job_id {
            self.persist_pure_v2_recheck_job(
                job_id,
                results.len() as i64,
                files.len() as i64,
                &invalid_files,
            );
        }
        if invalid_files.is_empty() {
            self.set_registry_state(info_hash, TorrentState::Seeding, Some(meta.total_length()))
                .await?;
        } else {
            self.set_registry_state(info_hash, TorrentState::Paused, None)
                .await?;
        }
        Ok(())
    }

    async fn set_registry_state(
        &self,
        info_hash: &str,
        state: TorrentState,
        completed_length: Option<u64>,
    ) -> CmdResult<()> {
        let mut reg = self.registry.write().await;
        let entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;
        if let Some(total) = completed_length {
            entry.total_length = total;
            entry.amount_left = 0;
            if entry.completed_at.is_none() {
                entry.completed_at = Some(unix_now_i64() as u64);
            }
        }
        let _ = entry.transition(state);
        let mut row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash).map_err(|e| e.to_string())?
        };
        row.state = entry.state.as_str().to_owned();
        row.completed_at = entry.completed_at.map(|value| value as i64);
        row.downloaded = entry
            .total_length
            .saturating_sub(entry.amount_left)
            .min(i64::MAX as u64) as i64;
        drop(reg);
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn update_torrent_labels_inner(
        &self,
        info_hash: &str,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        let mut reg = self.registry.write().await;
        let entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;

        if let Some(category) = category {
            entry.category = category.and_then(|value| normalize_category(Some(value)));
        }
        for tag in normalize_tags(add_tags) {
            if !entry.tags.contains(&tag) {
                entry.tags.push(tag);
            }
        }
        let remove_tags = normalize_tags(remove_tags);
        if !remove_tags.is_empty() {
            entry.tags.retain(|tag| !remove_tags.contains(tag));
        }
        let row = match load_meta_from_blob(&self.config, info_hash) {
            Ok(meta) => row_from_entry(entry, &meta),
            Err(_) => {
                let db = self.db.lock().expect("database mutex poisoned");
                let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
                row.category = entry.category.clone();
                row.tags = entry.tags.clone();
                row
            }
        };
        drop(reg);

        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            EVENT_LABELS_UPDATED,
            Some("torrent labels updated"),
            serde_json::json!({
                "category": row.category,
                "tags": row.tags,
            }),
        );
        Ok(())
    }

    async fn update_torrent_fields_inner(
        &self,
        info_hash: &str,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        let normalized_name = normalize_optional_text(name);
        let current_save_path = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            PathBuf::from(&entry.save_path)
        };
        let target_save_path = save_path;
        if let Some(target) = target_save_path.as_deref() {
            self.authorize_storage_path(target)?;
        }
        let meta = load_meta_from_blob(&self.config, info_hash).ok();
        if let (Some(target), Some(meta)) = (&target_save_path, meta.as_ref()) {
            if *target != current_save_path {
                if let Some(plan) =
                    self.plan_torrent_payload_files(&current_save_path, target, meta)?
                {
                    // The actor only performs bounded orchestration here.
                    // Filesystem work and checkpoints run behind the storage
                    // worker boundary; completion comes back as a command so
                    // health/lifecycle requests remain serviceable.
                    let quiesced = self.quiesce_torrent_for_storage_move(info_hash).await;
                    let (completion, completion_rx) = oneshot::channel();
                    let result = self.queue_storage_plan_job_with_context(
                        "move",
                        vec![info_hash.to_owned()],
                        &plan,
                        Vec::new(),
                        serde_json::json!({
                            "old_save_path": current_save_path.display().to_string(),
                            "save_path": target.display().to_string(),
                            "name": normalized_name.clone(),
                        }),
                        completion,
                    );
                    if let Ok(job_id) = &result {
                        let cmd_tx = self.cmd_tx.clone();
                        let job_id_for_task = job_id.clone();
                        let info_hash = info_hash.to_owned();
                        let name = normalized_name.clone();
                        let old_save_path = current_save_path.clone();
                        let save_path = target.clone();
                        tokio::spawn(async move {
                            let succeeded = completion_rx
                                .await
                                .map(|completion: StorageJobCompletion| completion.succeeded)
                                .unwrap_or(false);
                            let _ = cmd_tx
                                .send(EngineCmd::StorageMoveFinished {
                                    job_id: job_id_for_task,
                                    info_hash,
                                    name,
                                    old_save_path,
                                    save_path,
                                    quiesced,
                                    succeeded,
                                })
                                .await;
                        });
                        return Ok(Some(job_id.clone()));
                    }
                    self.resume_torrent_after_storage_move(info_hash, quiesced, None)
                        .await;
                    return result.map(|_| None);
                }
            }
        }

        let mut reg = self.registry.write().await;
        let entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;

        if let Some(name) = normalized_name {
            entry.name = name;
        }
        if let Some(save_path) = target_save_path {
            entry.save_path = save_path.to_string_lossy().to_string();
        }

        let row = match meta {
            Some(meta) => row_from_entry(entry, &meta),
            None => {
                let db = self.db.lock().expect("database mutex poisoned");
                let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
                row.name = entry.name.clone();
                row.save_path = entry.save_path.clone();
                row
            }
        };
        drop(reg);

        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": row.name,
                "save_path": row.save_path,
            }),
        );
        Ok(None)
    }

    fn plan_torrent_payload_files(
        &self,
        source_root: &std::path::Path,
        destination_root: &std::path::Path,
        meta: &TorrentMeta,
    ) -> CmdResult<Option<StoragePlan>> {
        self.authorize_storage_path(source_root)?;
        self.authorize_storage_path(destination_root)?;
        let mut steps = Vec::new();
        let mut rollback_steps = Vec::new();
        let mut issues = Vec::new();
        for (rel_path, bytes) in meta_file_entries(meta) {
            let source = rel_path.resolve(source_root);
            if !source.exists() {
                continue;
            }
            let destination = rel_path.resolve(destination_root);
            self.authorize_storage_path(&source)?;
            self.authorize_storage_path(&destination)?;
            let plan = rt_storage::plan_move(&rt_storage::MovePlanRequest {
                source,
                destination,
                bytes,
                available_bytes: None,
                dry_run: false,
            });
            issues.extend(plan.issues);
            steps.extend(plan.steps);
            rollback_steps.extend(plan.rollback_steps);
        }
        if steps.is_empty() {
            return Ok(None);
        }
        let plan = StoragePlan {
            dry_run: false,
            can_apply: issues.is_empty(),
            issues,
            steps,
            rollback_steps,
        };

        Ok(Some(plan))
    }

    /// Quiesces the running task for `info_hash` before a storage move, if
    /// one exists. Returns `Some(was_already_paused)` when a task was
    /// quiesced (the caller must resume it afterward via
    /// `resume_torrent_after_storage_move`), or `None` when there is no
    /// running task -- nothing to quiesce, and correspondingly nothing to
    /// resume.
    async fn quiesce_torrent_for_storage_move(&self, info_hash: &str) -> Option<bool> {
        let tx = self.torrent_chans.get(info_hash).cloned()?;
        let (reply, rx) = tokio::sync::oneshot::channel();
        if tx
            .send(TorrentCmd::QuiesceForStorageMove { reply })
            .await
            .is_err()
        {
            return None;
        }
        rx.await.ok()
    }

    /// Resumes a task previously quiesced by
    /// `quiesce_torrent_for_storage_move`. `new_save_root` should be
    /// `Some(destination)` when the move committed, `None` when it failed
    /// or was rolled back, so the task keeps using its original path
    /// unchanged. A no-op if `quiesced` is `None` (nothing was quiesced).
    async fn resume_torrent_after_storage_move(
        &self,
        info_hash: &str,
        quiesced: Option<bool>,
        new_save_root: Option<std::path::PathBuf>,
    ) {
        let Some(was_paused) = quiesced else {
            return;
        };
        if let Some(tx) = self.torrent_chans.get(info_hash).cloned() {
            let _ = tx
                .send(TorrentCmd::ResumeAfterStorageMove {
                    new_save_root,
                    resume_paused: was_paused,
                })
                .await;
        }
    }

    /// Quiesces every torrent in `info_hashes` that currently has a
    /// running task, for the duration of a generic (non-save-path-owning)
    /// storage plan execution. Returns the `(info_hash, was_already_paused)`
    /// pairs that actually got quiesced, for `resume_torrents_after_storage_plan`.
    async fn quiesce_torrents_for_storage_plan(
        &self,
        info_hashes: &[String],
    ) -> Vec<(String, bool)> {
        let mut quiesced = Vec::with_capacity(info_hashes.len());
        for info_hash in info_hashes {
            if let Some(was_paused) = self.quiesce_torrent_for_storage_move(info_hash).await {
                quiesced.push((info_hash.clone(), was_paused));
            }
        }
        quiesced
    }

    /// Resumes every torrent previously quiesced by
    /// `quiesce_torrents_for_storage_plan`. This generic executor never
    /// changes a torrent's canonical save_path, so every resume carries
    /// `new_save_root: None`.
    async fn resume_torrents_after_storage_plan(&self, quiesced: Vec<(String, bool)>) {
        for (info_hash, was_paused) in quiesced {
            self.resume_torrent_after_storage_move(&info_hash, Some(was_paused), None)
                .await;
        }
    }

    // Single call site (the storage-plan-job completion handler); each
    // parameter is a distinct piece of state needed to finish committing
    // or rolling back a move, not a natural grouping.
    #[allow(clippy::too_many_arguments)]
    async fn finish_storage_move(
        &mut self,
        job_id: &str,
        info_hash: &str,
        name: Option<String>,
        old_save_path: PathBuf,
        save_path: PathBuf,
        quiesced: Option<bool>,
        succeeded: bool,
    ) -> CmdResult<()> {
        if !succeeded {
            self.resume_torrent_after_storage_move(info_hash, quiesced, None)
                .await;
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move failed; save path unchanged"),
                serde_json::json!({
                    "job_id": job_id,
                    "save_path": old_save_path,
                    "storage_move": "failed",
                }),
            );
            return Ok(());
        }

        let meta = load_meta_from_blob(&self.config, info_hash).ok();
        let entry = {
            let mut registry = self.registry.write().await;
            let entry = registry
                .get_mut(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} disappeared during storage move"))?;
            if let Some(name) = name {
                entry.name = name;
            }
            entry.save_path = save_path.to_string_lossy().to_string();
            entry.clone()
        };
        let row = match meta {
            Some(meta) => row_from_entry(&entry, &meta),
            None => {
                let db = self.db.lock().expect("database mutex poisoned");
                let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
                row.name = entry.name.clone();
                row.save_path = entry.save_path.clone();
                row
            }
        };
        let persistence_error = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row)
                .err()
                .map(|error| error.to_string())
        };
        if let Some(error) = persistence_error {
            // Keep the in-memory projection aligned with the durable row if
            // persistence fails. The worker's file transaction is already
            // committed, so surface the failure loudly.
            let mut registry = self.registry.write().await;
            if let Some(entry) = registry.get_mut(info_hash) {
                entry.save_path = old_save_path.to_string_lossy().to_string();
            }
            return Err(error);
        }
        self.resume_torrent_after_storage_move(info_hash, quiesced, Some(save_path.clone()))
            .await;
        self.append_session_event(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated after storage move"),
            serde_json::json!({
                "job_id": job_id,
                "name": row.name,
                "save_path": row.save_path,
                "storage_move": "completed",
            }),
        );
        Ok(())
    }

    async fn update_torrent_trackers_inner(
        &self,
        info_hash: &str,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        let trackers = normalize_tracker_urls(trackers);
        let mut row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash).map_err(|e| e.to_string())?
        };
        row.trackers = trackers.clone();
        let tracker_rows = tracker_rows_from_urls(
            info_hash,
            &trackers,
            row.uploaded,
            row.downloaded,
            row.total_length.saturating_sub(row.downloaded).max(0),
        );

        {
            let mut db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
            rt_db::replace_torrent_trackers(&mut db, info_hash, &tracker_rows)
                .map_err(|e| e.to_string())?;
        }
        self.append_session_event(
            Some(info_hash),
            EVENT_TRACKERS_UPDATED,
            Some("torrent trackers updated"),
            serde_json::json!({ "trackers": trackers }),
        );
        Ok(())
    }

    async fn torrent_limits_inner(&self, info_hash: &str) -> CmdResult<EngineTorrentLimits> {
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        let db = self.db.lock().expect("database mutex poisoned");
        match rt_db::get_torrent_limits(&db, info_hash) {
            Ok(row) => Ok(engine_limits_from_row(row)),
            Err(rt_db::DbError::NotFound(_)) => Ok(EngineTorrentLimits::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn update_torrent_limits_inner(
        &self,
        info_hash: &str,
        limits: EngineTorrentLimits,
    ) -> CmdResult<()> {
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        let row = rt_db::TorrentLimitRow {
            info_hash: info_hash.to_owned(),
            download_limit: limits.download_limit.filter(|value| *value > 0),
            upload_limit: limits.upload_limit.filter(|value| *value > 0),
            max_connections: limits.max_connections.filter(|value| *value > 0),
            seed_ratio_limit: limits.seed_ratio_limit.filter(|value| *value >= 0.0),
            seed_idle_limit: limits.seed_idle_limit.filter(|value| *value >= 0),
            sequential_download: limits.sequential_download,
            sequential_download_from_piece: limits
                .sequential_download_from_piece
                .filter(|value| *value >= 0),
            first_last_piece_prio: limits.first_last_piece_prio,
            force_start: limits.force_start,
            super_seeding: limits.super_seeding,
            auto_tmm: limits.auto_tmm,
            auto_management: limits.auto_management,
        };
        {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert_torrent_limits(&db, &row).map_err(|e| e.to_string())?;
        }
        self.append_session_event(
            Some(info_hash),
            EVENT_LIMITS_UPDATED,
            Some("torrent limits updated"),
            serde_json::json!({
                "download_limit": row.download_limit,
                "upload_limit": row.upload_limit,
                "max_connections": row.max_connections,
                "seed_ratio_limit": row.seed_ratio_limit,
                "seed_idle_limit": row.seed_idle_limit,
                "sequential_download": row.sequential_download,
                "sequential_download_from_piece": row.sequential_download_from_piece,
                "first_last_piece_prio": row.first_last_piece_prio,
                "force_start": row.force_start,
                "super_seeding": row.super_seeding,
            }),
        );
        if let Some(tx) = self.torrent_chans.get(info_hash).cloned() {
            let _ = tx.send(TorrentCmd::UpdateLimits(limits)).await;
        }
        Ok(())
    }

    async fn update_file_priorities_inner(
        &self,
        info_hash: &str,
        file_ids: Vec<u32>,
        priority: i64,
    ) -> CmdResult<()> {
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        let priority = priority.clamp(0, 2);
        let wanted = priority > 0;
        let ids = file_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        {
            let mut db = self.db.lock().expect("database mutex poisoned");
            let mut files = rt_db::list_torrent_files(&db, info_hash).map_err(|e| e.to_string())?;
            if files.is_empty() {
                return Err(format!("torrent {info_hash} has no persisted files"));
            }
            let apply_all = ids.is_empty();
            let mut touched = 0usize;
            for file in &mut files {
                if apply_all || ids.contains(&(file.file_index as u32)) {
                    file.priority = priority;
                    file.wanted = wanted;
                    touched += 1;
                }
            }
            if touched == 0 {
                return Err(format!("no matching files for torrent {info_hash}"));
            }
            rt_db::replace_torrent_files(&mut db, info_hash, &files).map_err(|e| e.to_string())?;
        }
        self.append_session_event(
            Some(info_hash),
            "file_priorities_updated",
            Some("torrent file priorities updated"),
            serde_json::json!({
                "file_ids": ids.into_iter().collect::<Vec<_>>(),
                "priority": priority,
                "wanted": wanted,
            }),
        );
        let _ = self
            .send_to_torrent(info_hash, TorrentCmd::ReloadFilePolicy)
            .await;
        Ok(())
    }

    async fn add_peers_inner(&mut self, info_hash: &str, peers: Vec<SocketAddr>) -> CmdResult<()> {
        if peers.is_empty() {
            return Ok(());
        }
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        if self.is_taskless_pure_v2_torrent(info_hash) {
            return Err("pure v2 peer transfer is not implemented".to_owned());
        }
        let was_taskless = !self.torrent_chans.contains_key(info_hash);
        self.ensure_torrent_task(info_hash).await?;
        if was_taskless {
            self.send_to_torrent(info_hash, TorrentCmd::Resume).await?;
        }
        self.send_to_torrent(info_hash, TorrentCmd::PriorityPeers(peers))
            .await
    }

    async fn torrent_peers_inner(&self, info_hash: &str) -> CmdResult<Vec<EnginePeerSnapshot>> {
        let tx = self.torrent_chans.get(info_hash).cloned();
        let Some(tx) = tx else {
            let reg = self.registry.read().await;
            return if reg.get(info_hash).is_some() {
                Ok(Vec::new())
            } else {
                Err(format!("torrent {info_hash} not found"))
            };
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        tx.send(TorrentCmd::GetPeers { reply })
            .await
            .map_err(|_| "torrent task gone".to_owned())?;
        rx.await
            .map_err(|_| "torrent task dropped reply".to_owned())
    }

    async fn torrent_webseeds_inner(
        &self,
        info_hash: &str,
    ) -> CmdResult<Vec<EngineWebseedSnapshot>> {
        let tx = self.torrent_chans.get(info_hash).cloned();
        let Some(tx) = tx else {
            let meta = self
                .load_torrent_metadata(info_hash)
                .map_err(|e| e.to_string())?;
            return Ok(meta
                .webseeds
                .into_iter()
                .map(|url| EngineWebseedSnapshot {
                    url,
                    is_downloading: false,
                    download_rate: 0,
                    failures: 0,
                })
                .collect());
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        tx.send(TorrentCmd::GetWebseeds { reply })
            .await
            .map_err(|_| "torrent task gone".to_owned())?;
        rx.await
            .map_err(|_| "torrent task dropped reply".to_owned())
    }

    fn torrent_trackers_inner(&self, info_hash: &str) -> CmdResult<Vec<EngineTrackerSnapshot>> {
        let db = self.db.lock().expect("database mutex poisoned");
        let row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
        rt_db::list_torrent_trackers(&db, &row.info_hash)
            .map(|trackers| trackers.into_iter().map(engine_tracker_snapshot).collect())
            .map_err(|e| e.to_string())
    }

    async fn rename_file_path_inner(
        &self,
        info_hash: &str,
        file_id: u32,
        new_path: String,
    ) -> CmdResult<()> {
        self.ensure_torrent_exists(info_hash).await?;
        let new_path = normalize_relative_path(&new_path)?;
        let mut db = self.db.lock().expect("database mutex poisoned");
        let mut files = rt_db::list_torrent_files(&db, info_hash).map_err(|e| e.to_string())?;
        if files.is_empty() {
            return Err(format!("torrent {info_hash} has no persisted files"));
        }
        let Some(file) = files
            .iter_mut()
            .find(|file| file.file_index as u32 == file_id)
        else {
            return Err(format!("file {file_id} not found for torrent {info_hash}"));
        };
        file.path = new_path.clone();
        rt_db::replace_torrent_files(&mut db, info_hash, &files).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            "file_path_renamed",
            Some("torrent file path renamed"),
            serde_json::json!({ "file_id": file_id, "new_path": new_path }),
        );
        Ok(())
    }

    async fn rename_folder_path_inner(
        &self,
        info_hash: &str,
        old_path: String,
        new_path: String,
    ) -> CmdResult<()> {
        self.ensure_torrent_exists(info_hash).await?;
        let old_path = normalize_relative_path(&old_path)?;
        let new_path = normalize_relative_path(&new_path)?;
        let old_prefix = format!("{old_path}/");
        let mut db = self.db.lock().expect("database mutex poisoned");
        let mut files = rt_db::list_torrent_files(&db, info_hash).map_err(|e| e.to_string())?;
        if files.is_empty() {
            return Err(format!("torrent {info_hash} has no persisted files"));
        }
        let mut touched = 0usize;
        for file in &mut files {
            if file.path == old_path {
                file.path = new_path.clone();
                touched += 1;
            } else if let Some(rest) = file.path.strip_prefix(&old_prefix) {
                file.path = format!("{new_path}/{rest}");
                touched += 1;
            }
        }
        if touched == 0 {
            return Err(format!(
                "folder {old_path} not found for torrent {info_hash}"
            ));
        }
        rt_db::replace_torrent_files(&mut db, info_hash, &files).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            "folder_path_renamed",
            Some("torrent folder path renamed"),
            serde_json::json!({ "old_path": old_path, "new_path": new_path, "files": touched }),
        );
        Ok(())
    }

    async fn ensure_torrent_exists(&self, info_hash: &str) -> CmdResult<()> {
        let reg = self.registry.read().await;
        if reg.get(info_hash).is_some() {
            Ok(())
        } else {
            Err(format!("torrent {info_hash} not found"))
        }
    }

    fn global_limits_inner(&self) -> CmdResult<EngineGlobalLimits> {
        let db = self.db.lock().expect("database mutex poisoned");
        Ok(EngineGlobalLimits {
            download_limit: setting_i64(&db, SETTING_GLOBAL_DOWNLOAD_LIMIT),
            upload_limit: setting_i64(&db, SETTING_GLOBAL_UPLOAD_LIMIT),
            speed_limits_mode: setting_bool(&db, SETTING_GLOBAL_SPEED_LIMITS_MODE),
        })
    }

    fn apply_shared_global_limits_from_db(&self) {
        let limits = self.global_limits_inner().unwrap_or_default();
        self.apply_shared_global_limits(&limits);
    }

    fn apply_shared_global_limits(&self, limits: &EngineGlobalLimits) {
        let configured_download = (self.config.network.download_rate_limit > 0)
            .then_some(self.config.network.download_rate_limit);
        let configured_upload = (self.config.network.upload_rate_limit > 0)
            .then_some(self.config.network.upload_rate_limit);
        let download = if limits.speed_limits_mode && limits.download_limit > 0 {
            Some(limits.download_limit as u64)
        } else {
            configured_download
        };
        let upload = if limits.speed_limits_mode && limits.upload_limit > 0 {
            Some(limits.upload_limit as u64)
        } else {
            configured_upload
        };
        self.network_budget.set_download_limit(download);
        self.network_budget.set_upload_limit(upload);
    }

    fn update_global_limits_inner(&self, limits: EngineGlobalLimits) -> CmdResult<()> {
        let db = self.db.lock().expect("database mutex poisoned");
        let now = unix_now_i64();
        rt_db::set_setting(
            &db,
            SETTING_GLOBAL_DOWNLOAD_LIMIT,
            &limits.download_limit.max(0).to_string(),
            now,
        )
        .map_err(|e| e.to_string())?;
        rt_db::set_setting(
            &db,
            SETTING_GLOBAL_UPLOAD_LIMIT,
            &limits.upload_limit.max(0).to_string(),
            now,
        )
        .map_err(|e| e.to_string())?;
        rt_db::set_setting(
            &db,
            SETTING_GLOBAL_SPEED_LIMITS_MODE,
            if limits.speed_limits_mode { "1" } else { "0" },
            now,
        )
        .map_err(|e| e.to_string())?;
        self.apply_shared_global_limits(&limits);
        Ok(())
    }

    fn network_features_inner(&self) -> CmdResult<EngineNetworkFeatures> {
        let db = self.db.lock().expect("database mutex poisoned");
        Ok(EngineNetworkFeatures {
            dht: self.dht_tx.is_some()
                && setting_bool_with_default(&db, SETTING_NETWORK_DHT, self.config.dht.enabled),
            pex: setting_bool_with_default(&db, SETTING_NETWORK_PEX, true),
        })
    }

    async fn update_network_features_inner(
        &mut self,
        features: EngineNetworkFeatures,
    ) -> CmdResult<()> {
        {
            let db = self.db.lock().expect("database mutex poisoned");
            let now = unix_now_i64();
            rt_db::set_setting(
                &db,
                SETTING_NETWORK_DHT,
                if features.dht { "1" } else { "0" },
                now,
            )
            .map_err(|e| e.to_string())?;
            rt_db::set_setting(
                &db,
                SETTING_NETWORK_PEX,
                if features.pex { "1" } else { "0" },
                now,
            )
            .map_err(|e| e.to_string())?;
        }

        match (features.dht, self.dht_tx.is_some()) {
            (false, true) => {
                if let Some(tx) = self.dht_tx.take() {
                    shutdown_dht_task(tx, Duration::from_secs(10)).await;
                }
            }
            (true, false) => {
                let tx = spawn_dht_task(&self.config);
                self.dht_tx = Some(tx);
                self.register_all_dht_torrents().await;
            }
            _ => {}
        }
        for tx in self.torrent_chans.values() {
            let _ = tx.send(TorrentCmd::UpdatePeerExchange(features.pex)).await;
        }
        Ok(())
    }

    fn peer_exchange_enabled(&self) -> bool {
        let db = self.db.lock().expect("database mutex poisoned");
        setting_bool_with_default(&db, SETTING_NETWORK_PEX, true)
    }

    fn queue_priority_inner(&self, info_hash: &str) -> CmdResult<i32> {
        let db = self.db.lock().expect("database mutex poisoned");
        let key = queue_setting_key(info_hash);
        Ok(setting_i64(&db, &key) as i32)
    }

    async fn update_queue_order_inner(
        &self,
        info_hashes: Vec<String>,
        queue_move: QueueMove,
    ) -> CmdResult<()> {
        if info_hashes.is_empty() {
            return Ok(());
        }
        let mut rows = {
            let reg = self.registry.read().await;
            reg.iter().cloned().collect::<Vec<_>>()
        };
        rows.sort_by(|a, b| {
            a.added_at
                .cmp(&b.added_at)
                .then_with(|| a.info_hash.cmp(&b.info_hash))
        });
        let db = self.db.lock().expect("database mutex poisoned");
        let mut ordered = rows
            .into_iter()
            .map(|entry| {
                let pos = setting_i64(&db, &queue_setting_key(&entry.info_hash));
                (entry.info_hash, pos)
            })
            .collect::<Vec<_>>();
        ordered.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let known = ordered
            .iter()
            .map(|(hash, _)| hash.as_str())
            .collect::<std::collections::HashSet<_>>();
        if info_hashes
            .iter()
            .any(|hash| !known.contains(hash.as_str()))
        {
            return Err("one or more torrents not found".to_owned());
        }
        let selected = info_hashes
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut hashes = ordered
            .into_iter()
            .map(|(hash, _)| hash)
            .collect::<Vec<_>>();
        match queue_move {
            QueueMove::Top => {
                stable_partition_selected(&mut hashes, &selected, true);
            }
            QueueMove::Bottom => {
                stable_partition_selected(&mut hashes, &selected, false);
            }
            QueueMove::Up => {
                for idx in 1..hashes.len() {
                    if selected.contains(hashes[idx].as_str())
                        && !selected.contains(hashes[idx - 1].as_str())
                    {
                        hashes.swap(idx - 1, idx);
                    }
                }
            }
            QueueMove::Down => {
                for idx in (0..hashes.len().saturating_sub(1)).rev() {
                    if selected.contains(hashes[idx].as_str())
                        && !selected.contains(hashes[idx + 1].as_str())
                    {
                        hashes.swap(idx, idx + 1);
                    }
                }
            }
        }
        let now = unix_now_i64();
        for (idx, hash) in hashes.iter().enumerate() {
            rt_db::set_setting(
                &db,
                &queue_setting_key(hash),
                &(idx as i64).to_string(),
                now,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn send_to_torrent(&self, info_hash: &str, cmd: TorrentCmd) -> CmdResult<()> {
        match self.torrent_chans.get(info_hash) {
            Some(tx) => tx
                .send(cmd)
                .await
                .map_err(|_| "torrent task gone".to_owned()),
            None => Err(format!("torrent {info_hash} not found")),
        }
    }

    async fn engine_stats(&mut self) -> CmdResult<EngineStats> {
        if let Some((generated_at, cached)) = self.stats_cache.as_ref() {
            if generated_at.elapsed() <= ENGINE_STATS_CACHE_TTL {
                return Ok(cached.clone());
            }
        }
        let mut stats = EngineStats::default();
        let mut states = HashMap::new();
        {
            let reg = self.registry.read().await;
            for entry in reg.iter() {
                states.insert(entry.info_hash.clone(), entry.state);
                stats.torrents_total += 1;
                stats.bytes_uploaded = stats.bytes_uploaded.saturating_add(entry.stats.uploaded);
                stats.bytes_downloaded = stats
                    .bytes_downloaded
                    .saturating_add(entry.stats.downloaded);
                stats.bytes_left = stats.bytes_left.saturating_add(entry.amount_left);
                match entry.state {
                    TorrentState::Seeding => stats.torrents_seeding += 1,
                    TorrentState::Downloading => stats.torrents_downloading += 1,
                    TorrentState::Paused | TorrentState::Stopped => stats.torrents_paused += 1,
                    TorrentState::Checking => stats.torrents_checking += 1,
                    TorrentState::MetadataPending => stats.torrents_metadata_pending += 1,
                    TorrentState::Queued => stats.torrents_queued += 1,
                    TorrentState::Error => stats.torrents_error += 1,
                }
            }
        }
        let now = Instant::now();
        let trackers = {
            let db = self.db.lock().expect("database mutex poisoned");
            stats.jobs_active = rt_db::list_active_jobs(&db)
                .map_err(|e| e.to_string())?
                .len() as u64;
            rt_db::list_all_torrent_trackers(&db).map_err(|e| e.to_string())?
        };
        let storage_jobs = self.storage_jobs.stats();
        stats.storage_jobs_inflight = storage_jobs.inflight as u64;
        stats.storage_jobs_queue_depth = storage_jobs.queue_depth as u64;
        stats.storage_jobs_capacity = storage_jobs.capacity as u64;
        stats.storage_workers = storage_jobs.worker_count as u64;
        stats.trackers_total = trackers.len() as u64;
        for tracker in trackers {
            match tracker.status.as_str() {
                "working" => stats.trackers_working += 1,
                "warning" => stats.trackers_warning += 1,
                "error" => stats.trackers_error += 1,
                _ => {}
            }
        }
        if let Some(dht_tx) = &self.dht_tx {
            let (reply, rx) = tokio::sync::oneshot::channel();
            if dht_tx.send(DhtCommand::GetStats { reply }).await.is_ok() {
                match timeout(Duration::from_millis(250), rx).await {
                    Ok(Ok(dht)) => {
                        stats.dht_routing_nodes = dht.routing_nodes;
                        stats.dht_announced_peer_sets = dht.announced_peer_sets;
                        stats.dht_announced_peers = dht.announced_peers;
                        stats.dht_tracked_torrents = dht.tracked_torrents;
                        stats.dht_outstanding_requests = dht.outstanding_requests;
                        stats.dht_queried_nodes = dht.queried_nodes;
                    }
                    Ok(Err(_)) => {}
                    Err(_) => warn!(
                        component = "engine",
                        operation = "collect_runtime_stats",
                        target = "dht",
                        duration_ms = 250_u64,
                        result = "timeout",
                        "timed out collecting DHT runtime stats"
                    ),
                }
            }
        }
        // Query task actors in bounded parallelism. The old sequential loop
        // made a single slow/dead torrent add 250 ms to every other torrent;
        // at 100k tasks that was an outage-sized stats request.
        let task_channels = self
            .torrent_chans
            .iter()
            .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
            .collect::<Vec<_>>();
        let runtime_results =
            stream::iter(task_channels.into_iter().map(|(info_hash, tx)| async move {
                let (reply, rx) = tokio::sync::oneshot::channel();
                if tx
                    .send(TorrentCmd::GetRuntimeStats { reply })
                    .await
                    .is_err()
                {
                    return (info_hash, None);
                }
                match timeout(Duration::from_millis(250), rx).await {
                    Ok(Ok(runtime)) => (info_hash, Some(runtime)),
                    Ok(Err(_)) => (info_hash, None),
                    Err(_) => {
                        warn!(
                            component = "engine",
                            operation = "collect_runtime_stats",
                            target = "torrent",
                            torrent = %info_hash,
                            duration_ms = 250_u64,
                            result = "timeout",
                            "timed out collecting torrent runtime stats"
                        );
                        (info_hash, None)
                    }
                }
            }))
            .buffer_unordered(64)
            .collect::<Vec<_>>()
            .await;

        for (info_hash, runtime) in runtime_results {
            let Some(state) = states.remove(&info_hash) else {
                continue;
            };
            if let Some(runtime) = runtime {
                let tier = self
                    .tier_controller
                    .apply_input(
                        info_hash.clone(),
                        TierInput {
                            state,
                            connected_peers: runtime.connected_peers as usize,
                            outstanding_requests: runtime.outstanding_requests as usize,
                            inbound_peer: false,
                            tracker_due: false,
                            last_active: self.tier_last_active.get(&info_hash).copied(),
                            now,
                        },
                    )
                    .tier;
                stats.add_activity_tier(tier);
                stats.add_torrent_runtime(info_hash, runtime);
            } else {
                let tier = self.tier_controller.tier(&info_hash).unwrap_or_else(|| {
                    TierPolicy::default()
                        .decide(TierInput {
                            state,
                            connected_peers: 0,
                            outstanding_requests: 0,
                            inbound_peer: false,
                            tracker_due: false,
                            last_active: self.tier_last_active.get(&info_hash).copied(),
                            now,
                        })
                        .tier
                });
                stats.add_activity_tier(tier);
            }
        }
        for (info_hash, state) in states {
            let tier = self.tier_controller.tier(&info_hash).unwrap_or_else(|| {
                TierPolicy::default()
                    .decide(TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.tier_last_active.get(&info_hash).copied(),
                        now,
                    })
                    .tier
            });
            stats.add_activity_tier(tier);
        }
        stats.dormant_runtime_heap_bytes = self.tier_controller.dormant_heap_bytes() as u64;
        let mut resources = self.resources.snapshot();
        let storage = StorageRuntime::global();
        let storage_frame = MemoryClass::StorageFrame as usize;
        resources.classes[storage_frame].cap_bytes = storage.frame_cap_bytes();
        resources.classes[storage_frame].used_bytes = storage.frame_in_use_bytes();
        resources.classes[storage_frame].denied_allocations = storage.frame_denied_allocations();
        let piece_assembly = MemoryClass::PieceAssembly as usize;
        resources.classes[piece_assembly].used_bytes = stats.piece_assembly_bytes;
        let peer_buffer = MemoryClass::PeerBuffer as usize;
        resources.classes[peer_buffer].used_bytes = stats
            .peer_rx_buffer_bytes
            .saturating_add(stats.peer_tx_buffer_bytes)
            .saturating_add(stats.peer_command_queue_bytes);
        let tracker_peers = MemoryClass::TrackerPeers as usize;
        resources.classes[tracker_peers].used_bytes = stats.tracker_peer_cache_bytes;
        resources.classes[tracker_peers].denied_allocations = resources.classes[tracker_peers]
            .denied_allocations
            .saturating_add(stats.tracker_peer_cache_drops);
        let dht_table = MemoryClass::DhtTable as usize;
        resources.classes[dht_table].used_bytes = stats
            .dht_routing_nodes
            .saturating_mul(64)
            .saturating_add(stats.dht_announced_peers.saturating_mul(32))
            .saturating_add(stats.dht_queried_nodes.saturating_mul(32))
            .saturating_add(stats.dht_outstanding_requests.saturating_mul(64));
        let queued_disk = MemoryClass::QueuedDisk as usize;
        resources.classes[queued_disk].used_bytes = stats.storage_queued_disk_bytes;
        resources.total_used_bytes = resources
            .classes
            .iter()
            .fold(0u64, |total, class| total.saturating_add(class.used_bytes));
        resources.pressure = memory_pressure_for(
            resources.total_used_bytes,
            resources.total_cap_bytes,
            self.config.memory.pressure_constrained_pct,
            self.config.memory.pressure_critical_pct,
        );
        stats.resources = Some(resources);
        self.stats_cache = Some((Instant::now(), stats.clone()));
        Ok(stats)
    }

    async fn engine_subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        let dht_enabled = self.dht_tx.is_some();
        let dht_healthy = if let Some(dht_tx) = &self.dht_tx {
            let (reply, response) = oneshot::channel();
            if dht_tx.send(DhtCommand::GetStats { reply }).await.is_err() {
                false
            } else {
                matches!(
                    timeout(Duration::from_millis(250), response).await,
                    Ok(Ok(_))
                )
            }
        } else {
            true
        };
        Ok(EngineSubsystemHealth {
            storage_workers_healthy: self.storage_jobs.is_healthy(),
            dht_enabled,
            dht_healthy,
        })
    }

    async fn diagnose_torrent_inner(&self, info_hash: &str) -> CmdResult<TorrentDiagnostic> {
        let (state, bytes_left) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            (entry.state, entry.amount_left)
        };
        let taskless_v2 = self.is_taskless_pure_v2_torrent(info_hash);
        let (is_private, trackers, active_jobs) = {
            let db = self.db.lock().expect("database mutex poisoned");
            let row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
            let trackers = rt_db::list_torrent_trackers(&db, info_hash).unwrap_or_default();
            let active_jobs = rt_db::list_active_jobs(&db)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|job| job.affected_torrents.iter().any(|hash| hash == info_hash))
                .count();
            (row.is_private, trackers, active_jobs)
        };
        let tracker_errors = trackers
            .iter()
            .filter(|tracker| tracker.status == "error")
            .count();
        let tracker_warnings = trackers
            .iter()
            .filter(|tracker| tracker.status == "warning")
            .count();
        let mut reasons = Vec::new();
        let mut next_actions = Vec::new();
        if state == TorrentState::Seeding && bytes_left == 0 {
            reasons.push("torrent is already seeding".to_owned());
        } else {
            if taskless_v2 {
                reasons.push(
                    "pure v2 torrent has metadata but no active v2 peer transfer task".to_owned(),
                );
                next_actions.push("recheck local files or wait for v2 transfer support".to_owned());
            }
            match state {
                TorrentState::Paused | TorrentState::Stopped => {
                    reasons.push("torrent is paused or stopped".to_owned());
                    if !taskless_v2 {
                        next_actions.push("resume the torrent".to_owned());
                    }
                }
                TorrentState::Checking => {
                    reasons.push("torrent is currently checking pieces".to_owned());
                    next_actions.push("wait for the active recheck job to finish".to_owned());
                }
                TorrentState::MetadataPending => {
                    reasons.push("torrent is waiting for metadata".to_owned());
                    next_actions
                        .push("wait for metadata peers or add the .torrent file".to_owned());
                }
                TorrentState::Downloading | TorrentState::Queued => {
                    reasons.push(format!("{bytes_left} bytes are still missing"));
                }
                TorrentState::Error => {
                    reasons.push("torrent is in an error state".to_owned());
                    next_actions.push("inspect tracker and storage errors".to_owned());
                }
                TorrentState::Seeding => {}
            }
            if active_jobs > 0 {
                reasons.push(format!("{active_jobs} active job(s) affect this torrent"));
            }
            if tracker_errors > 0 {
                reasons.push(format!("{tracker_errors} tracker(s) are in error state"));
                next_actions.push("check tracker failure_reason values".to_owned());
            }
            if tracker_warnings > 0 {
                reasons.push(format!("{tracker_warnings} tracker(s) reported warnings"));
            }
            if is_private && trackers.is_empty() {
                reasons.push("private torrent has no persisted trackers".to_owned());
                next_actions.push("add a private tracker before expecting peers".to_owned());
            }
        }
        if next_actions.is_empty() && state != TorrentState::Seeding {
            next_actions.push("inspect torrent files, trackers, and active jobs".to_owned());
        }
        Ok(TorrentDiagnostic {
            info_hash: info_hash.to_owned(),
            state: state.as_str().to_owned(),
            is_private,
            bytes_left,
            active_jobs,
            tracker_errors,
            tracker_warnings,
            reasons,
            next_actions,
        })
    }

    async fn control_recheck_job(&self, job_id: &str, target_state: &str) -> CmdResult<()> {
        let job = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get_job(&db, job_id).map_err(|e| e.to_string())?
        };
        if job.kind == JOB_KIND_STORAGE_PLAN {
            let action = match target_state {
                JOB_STATE_PAUSED => StorageJobAction::Pause,
                JOB_STATE_RUNNING => StorageJobAction::Resume,
                JOB_STATE_CANCELLED => StorageJobAction::Cancel,
                _ => return Err(format!("unsupported job state {target_state}")),
            };
            if job.finished_at.is_some()
                || matches!(
                    job.state.as_str(),
                    JOB_STATE_CANCELLED | JOB_STATE_COMPLETED | JOB_STATE_FAILED
                )
            {
                return Err(format!("job {job_id} is already terminal"));
            }
            self.storage_jobs.control(job_id, action)?;
            self.update_job_state(
                job_id,
                target_state,
                None,
                Some("storage plan job control updated"),
            );
            return Ok(());
        }
        if job.kind != JOB_KIND_RECHECK {
            return Err(format!("job {job_id} is not a recheck job"));
        }
        if job.finished_at.is_some()
            || matches!(
                job.state.as_str(),
                JOB_STATE_CANCELLED | "completed" | JOB_STATE_FAILED
            )
        {
            return Err(format!("job {job_id} is already terminal"));
        }
        let info_hash = job
            .affected_torrents
            .first()
            .ok_or_else(|| format!("job {job_id} has no target torrent"))?
            .clone();
        let taskless_v2 = self.is_taskless_pure_v2_torrent(&info_hash);
        match target_state {
            JOB_STATE_PAUSED => {
                if taskless_v2 {
                    self.set_registry_state(&info_hash, TorrentState::Paused, None)
                        .await?;
                } else {
                    self.send_to_torrent(&info_hash, TorrentCmd::Pause).await?;
                }
                self.update_job_state(job_id, JOB_STATE_PAUSED, None, Some("recheck job paused"));
            }
            JOB_STATE_RUNNING => {
                if taskless_v2 {
                    self.recheck_pure_v2_torrent(&info_hash, Some(job_id.to_owned()))
                        .await?;
                } else {
                    self.send_to_torrent(
                        &info_hash,
                        TorrentCmd::Recheck {
                            job_id: Some(job_id.to_owned()),
                        },
                    )
                    .await?;
                    self.update_job_state(
                        job_id,
                        JOB_STATE_RUNNING,
                        None,
                        Some("recheck job resumed"),
                    );
                }
            }
            JOB_STATE_CANCELLED => {
                if !taskless_v2 {
                    self.send_to_torrent(
                        &info_hash,
                        TorrentCmd::CancelJob {
                            job_id: job_id.to_owned(),
                        },
                    )
                    .await?;
                }
                self.update_job_state(
                    job_id,
                    JOB_STATE_CANCELLED,
                    None,
                    Some("recheck job cancelled"),
                );
            }
            _ => return Err(format!("unsupported job state {target_state}")),
        }
        Ok(())
    }

    async fn ensure_metadata_task(&mut self, info_hash_hex: &str) -> CmdResult<()> {
        if self.torrent_chans.contains_key(info_hash_hex) {
            return Ok(());
        }
        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash_hex).map_err(|e| e.to_string())?
        };
        if !self.is_metadata_placeholder_row(&row) {
            return Err(format!("torrent {info_hash_hex} not running"));
        }
        let info_hash =
            parse_info_hash_hex(info_hash_hex).map_err(|_| "invalid info hash".to_owned())?;
        let _tx = self.spawn_metadata_task(
            info_hash,
            info_hash_hex.to_owned(),
            row.trackers,
            state_from_str(&row.state) == TorrentState::Paused,
        );
        Ok(())
    }

    /// Promote a persisted, taskless torrent into the hot runtime tier. A
    /// dormant seed keeps only its registry/SQLite/blob state; promotion
    /// reconstructs the task from the authoritative metainfo blob and then
    /// lets the normal command path resume/recheck it.
    async fn ensure_torrent_task(&mut self, info_hash_hex: &str) -> CmdResult<()> {
        if self.torrent_chans.contains_key(info_hash_hex) {
            return Ok(());
        }
        if self.metadata_placeholder_row(info_hash_hex).is_some() {
            return self.ensure_metadata_task(info_hash_hex).await;
        }
        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash_hex).map_err(|e| e.to_string())?
        };
        let raw = self
            .load_torrent_blob(info_hash_hex)
            .map_err(|e| e.to_string())?;
        let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
        let Some(v1) = meta_v1(meta) else {
            return Err("pure v2 peer transfer is not implemented".to_owned());
        };
        let info_hash = v1.info_hash;
        let is_private = v1.private;
        self.authorize_storage_path(Path::new(&row.save_path))?;
        let tier_key = info_hash_hex.to_owned();
        self.tier_controller.cancel_tracker_check(&tier_key);
        self.tier_controller.clear_dormant_snapshot(&tier_key);
        let _tx = self.spawn_torrent_task(tier_key, v1, PathBuf::from(row.save_path), true);
        self.tier_last_active
            .insert(info_hash_hex.to_owned(), Instant::now());
        if !is_private {
            self.register_dht_torrent(info_hash, info_hash_hex).await;
        }
        Ok(())
    }

    async fn register_dht_torrent(&self, info_hash: [u8; 20], info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Some(cmd_tx) = self.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        let _ = dht_tx
            .send(DhtCommand::AddTorrent(DhtTorrent { info_hash, cmd_tx }))
            .await;
    }

    async fn register_dht_torrent_from_blob(&self, info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Some(cmd_tx) = self.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        let raw = match std::fs::read(torrent_blob_path(&self.config, info_hash_hex)) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(
                    component = "dht",
                    operation = "register_torrent",
                    torrent = %info_hash_hex,
                    result = "error",
                    error = %e,
                    "failed to load torrent metadata for DHT registration"
                );
                return;
            }
        };
        let meta = match parse_torrent(&raw) {
            Ok(TorrentMeta::V1(m)) => m,
            Ok(TorrentMeta::Hybrid(m, _)) => *m,
            _ => return,
        };
        if meta.private {
            return;
        }
        let _ = dht_tx
            .send(DhtCommand::AddTorrent(DhtTorrent {
                info_hash: meta.info_hash,
                cmd_tx,
            }))
            .await;
    }

    async fn register_dht_torrent_from_storage_or_hash(&self, info_hash_hex: &str) {
        if std::fs::metadata(torrent_blob_path(&self.config, info_hash_hex)).is_ok() {
            self.register_dht_torrent_from_blob(info_hash_hex).await;
            return;
        }
        match parse_info_hash_hex(info_hash_hex) {
            Ok(info_hash) => self.register_dht_torrent(info_hash, info_hash_hex).await,
            Err(()) => {
                warn!(
                    component = "dht",
                    operation = "register_torrent",
                    torrent = %info_hash_hex,
                    result = "error",
                    "failed to parse info hash for DHT registration"
                )
            }
        }
    }

    async fn register_all_dht_torrents(&self) {
        let hashes = {
            let reg = self.registry.read().await;
            reg.iter()
                .map(|entry| entry.info_hash.clone())
                .collect::<Vec<_>>()
        };
        for hash in hashes {
            self.register_dht_torrent_from_storage_or_hash(&hash).await;
        }
    }

    fn is_metadata_placeholder_row(&self, row: &TorrentRow) -> bool {
        if state_from_str(&row.state) == TorrentState::MetadataPending {
            return true;
        }
        state_from_str(&row.state) == TorrentState::Paused
            && row.total_length == 0
            && row.piece_count == 0
            && std::fs::metadata(torrent_blob_path(&self.config, &row.info_hash)).is_err()
    }

    fn metadata_placeholder_row(&self, info_hash: &str) -> Option<TorrentRow> {
        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash).ok()?
        };
        self.is_metadata_placeholder_row(&row).then_some(row)
    }

    fn update_metadata_placeholder_state(
        &self,
        info_hash: &str,
        state: TorrentState,
    ) -> CmdResult<()> {
        let Some(mut row) = self.metadata_placeholder_row(info_hash) else {
            return Err(format!("torrent {info_hash} is not metadata pending"));
        };
        row.state = state.as_str().to_owned();
        {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        }
        if let Ok(mut registry) = self.registry.try_write() {
            if let Some(entry) = registry.get_mut(info_hash) {
                entry.state = state;
            }
        }
        Ok(())
    }

    async fn unregister_dht_torrent(&self, info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Ok(info_hash) = parse_info_hash_hex(info_hash_hex) else {
            return;
        };
        let _ = dht_tx.send(DhtCommand::RemoveTorrent(info_hash)).await;
    }

    fn append_session_event(
        &self,
        info_hash: Option<&str>,
        kind: &str,
        message: Option<&str>,
        payload: serde_json::Value,
    ) {
        let event = rt_db::SessionEventRow {
            event_id: None,
            occurred_at: unix_now_i64(),
            info_hash: info_hash.map(str::to_owned),
            kind: kind.to_owned(),
            message: message.map(str::to_owned),
            payload: payload.to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::append_session_event(&db, &event) {
            warn!(
                component = "db",
                operation = "append_session_event",
                kind,
                result = "error",
                error = %e,
                "failed to append session event"
            );
        } else if let Err(e) = rt_db::prune_session_events(&db, self.config.logging.event_retention)
        {
            warn!(
                component = "db",
                operation = "prune_session_events",
                kind,
                result = "error",
                error = %e,
                "failed to prune session events"
            );
        }
    }

    fn list_session_events(
        &self,
        info_hash: Option<&str>,
        kind: Option<&str>,
        levels: &[String],
        last_known_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<rt_db::SessionEventRow>, rt_db::DbError> {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_session_events_filtered(&db, info_hash, kind, levels, last_known_id, limit)
    }

    fn create_recheck_job(&self, info_hash: &str) -> Option<String> {
        let now = unix_now_i64();
        let total = self
            .load_torrent_metadata(info_hash)
            .map(|meta| meta.piece_count as i64)
            .unwrap_or(0);
        let seq = RECHECK_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("recheck-{info_hash}-{now}-{seq}");
        let job = rt_db::JobRow {
            job_id: job_id.clone(),
            kind: JOB_KIND_RECHECK.to_owned(),
            state: JOB_STATE_QUEUED.to_owned(),
            dry_run: false,
            affected_torrents: vec![info_hash.to_owned()],
            total,
            done: 0,
            checkpoint: 0,
            file_index: Some(0),
            piece_index: Some(0),
            byte_offset: Some(0),
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: now,
            started_at: None,
            updated_at: now,
            finished_at: None,
        };
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.clone(),
            occurred_at: now,
            kind: "job_queued".to_owned(),
            message: Some("recheck queued".to_owned()),
            payload: serde_json::json!({ "info_hash": info_hash }).to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(
                component = "db",
                operation = "persist_recheck_job",
                torrent = %info_hash,
                result = "error",
                error = %e,
                "failed to persist recheck job"
            );
            return None;
        }
        if let Err(e) = rt_db::append_job_event(&db, &event) {
            warn!(
                component = "db",
                operation = "append_job_event",
                job_id = %job_id,
                kind = %event.kind,
                result = "error",
                error = %e,
                "failed to append recheck job event"
            );
        }
        Some(job_id)
    }

    fn list_active_jobs(&self) -> CmdResult<Vec<EngineJob>> {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_active_jobs(&db)
            .map(|jobs| jobs.into_iter().map(EngineJob::from).collect())
            .map_err(|e| e.to_string())
    }

    fn list_storage_roots_inner(&self) -> CmdResult<Vec<EngineStorageRoot>> {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_storage_roots(&db)
            .map(|roots| roots.into_iter().map(engine_storage_root).collect())
            .map_err(|e| e.to_string())
    }

    fn configured_storage_roots_for_execution(&self) -> Result<Vec<PathBuf>, String> {
        self.configured_storage_authority()
            .map(ServerStorageRoots::into_roots)
    }

    fn configured_storage_authority(&self) -> Result<ServerStorageRoots, String> {
        let paths = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_storage_roots(&db)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|root| PathBuf::from(root.path))
                .collect::<Vec<_>>()
        };
        ServerStorageRoots::from_configured_paths(paths).map_err(|e| e.to_string())
    }

    fn authorize_storage_path(&self, path: &Path) -> Result<(), String> {
        let roots = self.configured_storage_roots_for_execution()?;
        let authority =
            ServerStorageRoots::from_configured_paths(roots).map_err(|e| e.to_string())?;
        authority.authorize_path(path).map_err(|e| e.to_string())
    }

    /// Repair only missing tracker detail rows during startup. Normal writes
    /// keep the summary JSON and detail table together; the one bulk query
    /// here preserves compatibility with older databases without performing
    /// a SELECT/DELETE/INSERT cycle for every restored torrent.
    fn repair_missing_torrent_tracker_rows(&self, rows: &[TorrentRow]) -> anyhow::Result<()> {
        let existing = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_all_torrent_trackers(&db)?
                .into_iter()
                .map(|tracker| tracker.info_hash)
                .collect::<HashSet<_>>()
        };
        let mut db = self.db.lock().expect("database mutex poisoned");
        for row in rows {
            if row.trackers.is_empty() || existing.contains(&row.info_hash) {
                continue;
            }
            let trackers = tracker_rows_from_urls(
                &row.info_hash,
                &row.trackers,
                row.uploaded,
                row.downloaded,
                row.total_length.saturating_sub(row.downloaded).max(0),
            );
            rt_db::replace_torrent_trackers(&mut db, &row.info_hash, &trackers)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn completed_storage_plan_steps(&self, job_id: &str) -> Vec<usize> {
        let db = self.db.lock().expect("database mutex poisoned");
        let Ok(job) = rt_db::get_job(&db, job_id) else {
            return Vec::new();
        };
        if job.kind != JOB_KIND_STORAGE_PLAN {
            return Vec::new();
        }
        let checkpoint = job.checkpoint.max(0) as usize;
        (0..checkpoint).collect()
    }

    fn update_job_state(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        message: Option<&str>,
    ) {
        let now = unix_now_i64();
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get_job(&db, job_id) {
                Ok(job) => job,
                Err(e) => {
                    warn!(
                        component = "db",
                        operation = "load_job_for_state_update",
                        job_id,
                        result = "error",
                        error = %e,
                        "failed to load job for state update"
                    );
                    return;
                }
            }
        };
        job.state = state.to_owned();
        job.error = error;
        job.updated_at = now;
        if state == JOB_STATE_RUNNING && job.started_at.is_none() {
            job.started_at = Some(now);
        }
        if matches!(state, JOB_STATE_CANCELLED | JOB_STATE_FAILED) {
            job.finished_at = Some(now);
        }
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: format!("job_{state}"),
            message: message.map(str::to_owned),
            payload: serde_json::json!({ "state": state }).to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(
                component = "db",
                operation = "persist_job_state",
                job_id,
                state,
                result = "error",
                error = %e,
                "failed to persist job state"
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
                "failed to append job state event"
            );
        }
        if state == JOB_STATE_RUNNING {
            let started = rt_db::JobEventRow {
                event_id: None,
                job_id: job_id.to_owned(),
                occurred_at: now,
                kind: "check_started".to_owned(),
                message: Some("recheck started".to_owned()),
                payload: serde_json::json!({ "state": state }).to_string(),
            };
            if let Err(e) = rt_db::append_job_event(&db, &started) {
                warn!(
                    component = "db",
                    operation = "append_job_event",
                    job_id,
                    kind = %started.kind,
                    result = "error",
                    error = %e,
                    "failed to append recheck start event"
                );
            }
        }
    }

    fn persist_pure_v2_recheck_job(
        &self,
        job_id: &str,
        done: i64,
        total: i64,
        invalid_files: &[i64],
    ) {
        let now = unix_now_i64();
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get_job(&db, job_id) {
                Ok(job) => job,
                Err(e) => {
                    warn!(
                        component = "db",
                        operation = "load_pure_v2_recheck_job",
                        job_id,
                        result = "error",
                        error = %e,
                        "failed to load pure v2 recheck job"
                    );
                    return;
                }
            }
        };
        job.total = total;
        job.done = done;
        job.checkpoint = done;
        job.file_index = Some(done);
        job.piece_index = None;
        job.byte_offset = None;
        job.invalid_pieces = invalid_files.to_vec();
        job.state = JOB_STATE_COMPLETED.to_owned();
        job.updated_at = now;
        job.finished_at = Some(now);
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: "check_completed".to_owned(),
            message: Some("pure v2 file-root recheck completed".to_owned()),
            payload: serde_json::json!({
                "done": done,
                "total": total,
                "invalid_files": invalid_files,
                "state": JOB_STATE_COMPLETED,
            })
            .to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(
                component = "db",
                operation = "persist_pure_v2_recheck_job",
                job_id,
                result = "error",
                error = %e,
                "failed to persist pure v2 recheck job"
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
                "failed to append pure v2 recheck event"
            );
        }
    }

    #[allow(dead_code)]
    fn create_storage_plan_job(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
    ) -> Result<String, String> {
        self.create_storage_plan_job_with_context(
            operation,
            affected_torrents,
            plan,
            serde_json::json!({}),
        )
    }

    fn create_storage_plan_job_with_context(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
        context: serde_json::Value,
    ) -> Result<String, String> {
        let now = unix_now_i64();
        let seq = STORAGE_PLAN_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("storage-plan-{operation}-{now}-{seq}");
        let job = rt_db::JobRow {
            job_id: job_id.clone(),
            kind: JOB_KIND_STORAGE_PLAN.to_owned(),
            state: JOB_STATE_QUEUED.to_owned(),
            dry_run: plan.dry_run,
            affected_torrents,
            total: plan.steps.len() as i64,
            done: 0,
            checkpoint: 0,
            file_index: Some(0),
            piece_index: None,
            byte_offset: Some(0),
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: now,
            started_at: None,
            updated_at: now,
            finished_at: None,
        };
        let mut payload = storage_plan_payload(operation, plan, &[]);
        if let Some(object) = payload.as_object_mut() {
            object.insert("context".to_owned(), context);
        }
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.clone(),
            occurred_at: now,
            kind: "storage_plan_queued".to_owned(),
            message: Some(format!("{operation} storage plan queued")),
            payload: payload.to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert_job(&db, &job).map_err(|e| e.to_string())?;
        rt_db::append_job_event(&db, &event).map_err(|e| e.to_string())?;
        Ok(job_id)
    }

    /// Queue a storage plan and return immediately. Filesystem work belongs
    /// to `StorageJobDispatcher`; awaiting the plan here would stop the
    /// engine actor from serving health, torrent, and control commands.
    fn queue_storage_plan_job(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
        completed_steps: Vec<usize>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<String, String> {
        self.queue_storage_plan_job_with_context(
            operation,
            affected_torrents,
            plan,
            completed_steps,
            serde_json::json!({}),
            completion,
        )
    }

    fn queue_storage_plan_job_with_context(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
        completed_steps: Vec<usize>,
        context: serde_json::Value,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<String, String> {
        let completed_steps = normalize_storage_plan_completed_steps(plan, completed_steps)?;
        let server_roots = self.configured_storage_roots_for_execution()?;
        let job_id =
            self.create_storage_plan_job_with_context(operation, affected_torrents, plan, context)?;
        if let Err(error) = self.storage_jobs.submit(
            Arc::clone(&self.db),
            job_id.clone(),
            operation.to_owned(),
            plan.clone(),
            completed_steps,
            server_roots,
            completion,
        ) {
            self.update_job_state(
                &job_id,
                JOB_STATE_FAILED,
                Some(error.clone()),
                Some("storage plan could not be queued"),
            );
            return Err(error);
        }
        Ok(job_id)
    }

    #[allow(dead_code)]
    fn execute_storage_plan_job(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
        completed_steps: Vec<usize>,
    ) -> Result<String, String> {
        let completed_steps = normalize_storage_plan_completed_steps(plan, completed_steps)?;
        let server_roots = self.configured_storage_roots_for_execution()?;
        let job_id = self.create_storage_plan_job(operation, affected_torrents, plan)?;
        self.update_job_state(
            &job_id,
            JOB_STATE_RUNNING,
            None,
            Some("storage plan execution started"),
        );
        let mut completed = completed_steps;
        let already_completed = completed.clone();
        let checkpoint = |index, _step: &StoragePlanStep| {
            if !completed.contains(&index) {
                completed.push(index);
                completed.sort_unstable();
            }
            self.persist_storage_plan_checkpoint(&job_id, operation, plan, &completed)
                .map_err(|error| StorageError::StagedMoveFailed {
                    step: "checkpoint",
                    reason: error,
                })
        };
        let result = rt_storage::execute_storage_plan_under_roots_with_checkpoints(
            plan,
            &server_roots,
            &already_completed,
            checkpoint,
        );
        match result {
            Ok(_) => {
                self.persist_storage_plan_terminal(
                    &job_id,
                    operation,
                    plan,
                    &completed,
                    JOB_STATE_COMPLETED,
                    None,
                )?;
                Ok(job_id)
            }
            Err(error) => {
                let message = error.to_string();
                self.persist_storage_plan_terminal(
                    &job_id,
                    operation,
                    plan,
                    &completed,
                    JOB_STATE_FAILED,
                    Some(message.clone()),
                )?;
                Err(message)
            }
        }
    }

    #[allow(dead_code)]
    fn persist_storage_plan_checkpoint(
        &self,
        job_id: &str,
        operation: &str,
        plan: &StoragePlan,
        completed_steps: &[usize],
    ) -> Result<(), String> {
        let now = unix_now_i64();
        let done = completed_steps.len() as i64;
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get_job(&db, job_id).map_err(|e| e.to_string())?
        };
        job.done = done;
        job.checkpoint = done;
        job.file_index = Some(done);
        job.byte_offset = Some(
            completed_steps
                .iter()
                .filter_map(|index| plan.steps.get(*index))
                .map(|step| step.bytes as i64)
                .sum::<i64>(),
        );
        job.updated_at = now;
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: "storage_plan_checkpoint".to_owned(),
            message: Some("storage plan checkpoint persisted".to_owned()),
            payload: storage_plan_payload(operation, plan, completed_steps).to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert_job(&db, &job).map_err(|e| e.to_string())?;
        rt_db::append_job_event(&db, &event).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[allow(dead_code)]
    fn persist_storage_plan_terminal(
        &self,
        job_id: &str,
        operation: &str,
        plan: &StoragePlan,
        completed_steps: &[usize],
        state: &str,
        error: Option<String>,
    ) -> Result<(), String> {
        let now = unix_now_i64();
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get_job(&db, job_id).map_err(|e| e.to_string())?
        };
        job.state = state.to_owned();
        job.done = completed_steps.len() as i64;
        job.checkpoint = job.done;
        job.error = error.clone();
        job.updated_at = now;
        job.finished_at = Some(now);
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: format!("storage_plan_{state}"),
            message: Some(format!("storage plan {state}")),
            payload: serde_json::json!({
                "error": error,
                "state": state,
                "plan": storage_plan_payload(operation, plan, completed_steps),
            })
            .to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert_job(&db, &job).map_err(|e| e.to_string())?;
        rt_db::append_job_event(&db, &event).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[allow(dead_code)]
fn decode_storage_plan_event(
    payload: &str,
) -> Option<(String, StoragePlan, Vec<usize>, serde_json::Value)> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let operation = value.get("operation")?.as_str()?.to_owned();
    let plan = serde_json::from_value(value.get("plan")?.clone()).ok()?;
    let completed_steps = value
        .get("completed_steps")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let context = value
        .get("context")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some((operation, plan, completed_steps, context))
}

#[allow(dead_code)]
fn normalize_storage_plan_completed_steps(
    plan: &StoragePlan,
    mut completed_steps: Vec<usize>,
) -> Result<Vec<usize>, String> {
    completed_steps.sort_unstable();
    completed_steps.dedup();
    if let Some(index) = completed_steps
        .iter()
        .copied()
        .find(|index| *index >= plan.steps.len())
    {
        return Err(format!(
            "completed storage-plan step {index} is outside plan length {}",
            plan.steps.len()
        ));
    }
    Ok(completed_steps)
}

fn recovered_storage_plan_steps(
    plan: &StoragePlan,
    checkpoint: i64,
    event_completed_steps: Vec<usize>,
) -> Result<Vec<usize>, String> {
    // The durable job projection stores a count, not the step indexes. Prefer
    // the event's exact sparse list; only use the prefix fallback for a crash
    // between the job-row update and its checkpoint-event insert.
    if event_completed_steps.is_empty() {
        return Ok(if checkpoint > 0 {
            (0..checkpoint as usize).collect()
        } else {
            Vec::new()
        });
    }
    normalize_storage_plan_completed_steps(plan, event_completed_steps)
}

#[allow(dead_code)]
fn storage_plan_payload(
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> serde_json::Value {
    serde_json::json!({
        "operation": operation,
        "plan": plan,
        "dry_run": plan.dry_run,
        "can_apply": plan.can_apply,
        "completed_steps": completed_steps,
        "steps": plan.steps.iter().map(storage_plan_step_payload_json).collect::<Vec<_>>(),
        "rollback_steps": plan.rollback_steps.iter().map(storage_plan_step_payload_json).collect::<Vec<_>>(),
        "issues": plan.issues.iter().map(|issue| format!("{issue:?}")).collect::<Vec<_>>(),
    })
}

#[allow(dead_code)]
fn storage_plan_step_payload_json(step: &StoragePlanStep) -> serde_json::Value {
    serde_json::json!({
        "action": format!("{:?}", step.action),
        "source": step.source.as_ref().map(|path| path.display().to_string()),
        "destination": step.destination.as_ref().map(|path| path.display().to_string()),
        "bytes": step.bytes,
    })
}

pub(crate) fn row_from_entry(entry: &TorrentEntry, meta: &TorrentMeta) -> TorrentRow {
    TorrentRow {
        info_hash: entry.info_hash.clone(),
        name: entry.name.clone(),
        total_length: meta_total_length(meta) as i64,
        piece_length: meta_piece_length(meta) as i64,
        piece_count: meta_piece_count(meta) as i64,
        is_private: meta.is_private(),
        save_path: entry.save_path.clone(),
        category: entry.category.clone(),
        tags: entry.tags.clone(),
        state: entry.state.as_str().to_owned(),
        added_at: entry.added_at as i64,
        completed_at: entry.completed_at.map(|t| t as i64),
        uploaded: entry.stats.uploaded as i64,
        downloaded: entry.stats.downloaded as i64,
        ratio: entry.stats.ratio(),
        trackers: meta_all_trackers(meta),
    }
}

fn persist_torrent_files(
    db: &mut Connection,
    info_hash: &str,
    meta: &TorrentMeta,
) -> anyhow::Result<()> {
    let rows = meta_file_rows(info_hash, meta);
    rt_db::replace_torrent_files(db, info_hash, &rows)?;
    Ok(())
}

fn sync_torrent_trackers_if_urls_changed(
    db: &mut Connection,
    row: &TorrentRow,
) -> anyhow::Result<()> {
    let existing = rt_db::list_torrent_trackers(db, &row.info_hash)?;
    let existing_urls = existing
        .iter()
        .map(|tracker| tracker.url.as_str())
        .collect::<Vec<_>>();
    let row_urls = row.trackers.iter().map(String::as_str).collect::<Vec<_>>();
    if existing_urls == row_urls {
        return Ok(());
    }
    let trackers = tracker_rows_from_urls(
        &row.info_hash,
        &row.trackers,
        row.uploaded,
        row.downloaded,
        row.total_length.saturating_sub(row.downloaded).max(0),
    );
    rt_db::replace_torrent_trackers(db, &row.info_hash, &trackers)?;
    Ok(())
}

fn tracker_rows_from_urls(
    info_hash: &str,
    trackers: &[String],
    uploaded: i64,
    downloaded: i64,
    left_bytes: i64,
) -> Vec<rt_db::TorrentTrackerRow> {
    trackers
        .iter()
        .enumerate()
        .map(|(idx, url)| rt_db::TorrentTrackerRow {
            info_hash: info_hash.to_owned(),
            tracker_index: idx as i64,
            tier: idx as i64,
            url: url.clone(),
            status: "pending".to_owned(),
            last_announce_at: None,
            next_announce_at: None,
            last_success_at: None,
            failure_reason: None,
            warning_message: None,
            seeders: None,
            leechers: None,
            completed: None,
            uploaded,
            downloaded,
            left_bytes,
        })
        .collect()
}

fn entry_from_row(row: &TorrentRow) -> TorrentEntry {
    TorrentEntry {
        handle: Default::default(),
        info_hash: row.info_hash.clone(),
        name: row.name.clone(),
        save_path: row.save_path.clone(),
        total_length: row.total_length.max(0) as u64,
        amount_left: if state_from_str(&row.state) == TorrentState::Seeding {
            0
        } else {
            (row.total_length.max(0) as u64).saturating_sub(row.downloaded.max(0) as u64)
        },
        state: state_from_str(&row.state),
        stats: TransferStats {
            uploaded: row.uploaded.max(0) as u64,
            downloaded: row.downloaded.max(0) as u64,
        },
        added_at: row.added_at.max(0) as u64,
        completed_at: row.completed_at.map(|t| t.max(0) as u64),
        category: row.category.clone(),
        tags: row.tags.clone(),
        error_message: None,
        tracker_message: None,
    }
}

fn state_from_str(state: &str) -> TorrentState {
    match state {
        "checking" => TorrentState::Checking,
        "metadata_pending" => TorrentState::MetadataPending,
        "seeding" => TorrentState::Seeding,
        "downloading" => TorrentState::Downloading,
        "paused" => TorrentState::Paused,
        "queued" => TorrentState::Queued,
        "error" => TorrentState::Error,
        _ => TorrentState::Stopped,
    }
}

fn should_start_task_on_restore(state: TorrentState) -> bool {
    matches!(
        state,
        TorrentState::Downloading | TorrentState::Checking | TorrentState::MetadataPending
    )
}

fn dormant_snapshot_from_row(
    row: &TorrentRow,
    state: TorrentState,
    tracker_deadline: Option<Instant>,
) -> DormantTorrentSnapshot {
    let piece_count = row.piece_count.max(0).min(u32::MAX as i64) as u32;
    dormant_snapshot_from_fields(&row.info_hash, state, piece_count, tracker_deadline)
}

fn dormant_snapshot_from_fields(
    info_hash: &str,
    state: TorrentState,
    piece_count: u32,
    tracker_deadline: Option<Instant>,
) -> DormantTorrentSnapshot {
    let pieces = if state == TorrentState::Seeding {
        CompactPieceBitmap::complete(piece_count)
    } else {
        CompactPieceBitmap::missing(piece_count)
    };
    DormantTorrentSnapshot::new(info_hash.to_owned(), state, pieces, tracker_deadline, None)
}

fn is_v2_only_placeholder_row(row: &TorrentRow) -> bool {
    row.info_hash.len() == 64 && row.total_length == 0 && row.piece_count == 0
}

fn engine_tracker_snapshot(row: rt_db::TorrentTrackerRow) -> EngineTrackerSnapshot {
    EngineTrackerSnapshot {
        id: row.tracker_index,
        tier: row.tier,
        announce: row.url,
        status: row.status,
        last_announce_at: row.last_announce_at,
        next_announce_at: row.next_announce_at,
        last_success_at: row.last_success_at,
        failure_reason: row.failure_reason,
        warning_message: row.warning_message,
        seeders: row.seeders,
        leechers: row.leechers,
        completed: row.completed,
    }
}

fn torrent_blob_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("torrents")
}

fn torrent_blob_path(config: &Config, info_hash: &str) -> PathBuf {
    torrent_blob_dir(config).join(format!("{info_hash}.torrent"))
}

fn load_meta_from_blob(config: &Config, info_hash: &str) -> anyhow::Result<TorrentMeta> {
    let raw = std::fs::read(torrent_blob_path(config, info_hash))?;
    Ok(parse_torrent(&raw)?)
}

fn meta_v1(meta: TorrentMeta) -> Option<TorrentMetaV1> {
    match meta {
        TorrentMeta::V1(meta) => Some(meta),
        TorrentMeta::Hybrid(meta, _) => Some(*meta),
        TorrentMeta::V2(_) => None,
    }
}

fn meta_raw(meta: &TorrentMeta) -> &[u8] {
    match meta {
        TorrentMeta::V1(meta) => &meta.raw,
        TorrentMeta::V2(meta) => &meta.raw,
        TorrentMeta::Hybrid(meta, _) => &meta.raw,
    }
}

fn meta_info_hash_hex(meta: &TorrentMeta) -> String {
    match meta {
        TorrentMeta::V1(meta) => hex::encode(meta.info_hash),
        TorrentMeta::V2(meta) => hex::encode(meta.info_hash_v2),
        TorrentMeta::Hybrid(meta, _) => hex::encode(meta.info_hash),
    }
}

fn meta_total_length(meta: &TorrentMeta) -> u64 {
    match meta {
        TorrentMeta::V1(meta) => meta.total_length(),
        TorrentMeta::V2(meta) => meta.total_length(),
        TorrentMeta::Hybrid(meta, _) => meta.total_length(),
    }
}

fn meta_piece_length(meta: &TorrentMeta) -> u64 {
    match meta {
        TorrentMeta::V1(meta) => meta.piece_length,
        TorrentMeta::V2(meta) => meta.piece_length,
        TorrentMeta::Hybrid(meta, _) => meta.piece_length,
    }
}

fn meta_piece_count(meta: &TorrentMeta) -> usize {
    match meta {
        TorrentMeta::V1(meta) => meta.pieces.len(),
        TorrentMeta::V2(meta) => meta.total_length().div_ceil(meta.piece_length) as usize,
        TorrentMeta::Hybrid(meta, _) => meta.pieces.len(),
    }
}

fn meta_all_trackers(meta: &TorrentMeta) -> Vec<String> {
    match meta {
        TorrentMeta::V1(meta) => meta.all_trackers(),
        TorrentMeta::Hybrid(meta, _) => meta.all_trackers(),
        TorrentMeta::V2(meta) => v2_all_trackers(meta),
    }
}

fn v2_all_trackers(meta: &TorrentMetaV2) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    if let Some(announce) = &meta.announce {
        if seen.insert(announce.clone()) {
            out.push(announce.clone());
        }
    }
    for tier in &meta.announce_list {
        for url in tier {
            if seen.insert(url.clone()) {
                out.push(url.clone());
            }
        }
    }
    out
}

fn meta_file_paths(meta: &TorrentMeta) -> Vec<rt_path::SafeRelPath> {
    match meta {
        TorrentMeta::V1(meta) => meta.files.iter().map(|file| file.path.clone()).collect(),
        TorrentMeta::Hybrid(meta, _) => meta.files.iter().map(|file| file.path.clone()).collect(),
        TorrentMeta::V2(meta) => meta.files.iter().map(|file| file.path.clone()).collect(),
    }
}

fn meta_file_entries(meta: &TorrentMeta) -> Vec<(rt_path::SafeRelPath, u64)> {
    match meta {
        TorrentMeta::V1(meta) => meta
            .files
            .iter()
            .map(|file| (file.path.clone(), file.length))
            .collect(),
        TorrentMeta::Hybrid(meta, _) => meta
            .files
            .iter()
            .map(|file| (file.path.clone(), file.length))
            .collect(),
        TorrentMeta::V2(meta) => meta
            .files
            .iter()
            .map(|file| (file.path.clone(), file.length))
            .collect(),
    }
}

fn meta_file_rows(info_hash: &str, meta: &TorrentMeta) -> Vec<rt_db::TorrentFileRow> {
    match meta {
        TorrentMeta::V1(meta) => meta
            .files
            .iter()
            .map(|file| rt_db::TorrentFileRow {
                info_hash: info_hash.to_owned(),
                file_index: file.index as i64,
                path: file.path.as_display(),
                length: file.length as i64,
                offset: file.offset as i64,
                priority: 1,
                wanted: true,
                completed_bytes: 0,
            })
            .collect(),
        TorrentMeta::Hybrid(meta, _) => meta
            .files
            .iter()
            .map(|file| rt_db::TorrentFileRow {
                info_hash: info_hash.to_owned(),
                file_index: file.index as i64,
                path: file.path.as_display(),
                length: file.length as i64,
                offset: file.offset as i64,
                priority: 1,
                wanted: true,
                completed_bytes: 0,
            })
            .collect(),
        TorrentMeta::V2(meta) => meta
            .files
            .iter()
            .map(|file| rt_db::TorrentFileRow {
                info_hash: info_hash.to_owned(),
                file_index: file.index as i64,
                path: file.path.as_display(),
                length: file.length as i64,
                offset: file.offset as i64,
                priority: 1,
                wanted: true,
                completed_bytes: 0,
            })
            .collect(),
    }
}

fn fastresume_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("fastresume")
}

fn register_configured_storage(conn: &Connection, config: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.storage.download_dir)?;
    let path = normalized_path_string(&config.storage.download_dir);
    let now = unix_now_i64();
    let root_id = stable_id("root", &path);
    let mount_id = stable_mount_id(&config.storage.download_dir);
    rt_db::upsert_storage_root(
        conn,
        &rt_db::StorageRootRow {
            root_id,
            path: path.clone(),
            profile: "auto".to_owned(),
            created_at: now,
        },
    )?;
    rt_db::upsert_mount(
        conn,
        &rt_db::MountRow {
            mount_id,
            path,
            fs_type: Some("unknown".to_owned()),
            device: storage_device_id(&config.storage.download_dir),
            queue_depth: 1,
            read_concurrency: 1,
            write_concurrency: 1,
            updated_at: now,
        },
    )?;
    Ok(())
}

fn normalized_path_string(path: &std::path::Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn engine_storage_root(row: rt_db::StorageRootRow) -> EngineStorageRoot {
    let path = PathBuf::from(&row.path);
    match storage_capacity(&path) {
        Ok((total_bytes, available_bytes)) => EngineStorageRoot {
            id: row.root_id,
            path,
            profile: row.profile,
            total_bytes,
            available_bytes,
            used_bytes: total_bytes.saturating_sub(available_bytes),
            ok: true,
            error: None,
        },
        Err(error) => EngineStorageRoot {
            id: row.root_id,
            path,
            profile: row.profile,
            total_bytes: 0,
            available_bytes: 0,
            used_bytes: 0,
            ok: false,
            error: Some(error),
        },
    }
}

#[cfg(unix)]
fn storage_capacity(path: &std::path::Path) -> Result<(u64, u64), String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        format!(
            "storage root path contains an interior NUL: {}",
            path.display()
        )
    })?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(format!(
            "statvfs failed for {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize.max(stat.f_bsize);
    let total = stat.f_blocks.saturating_mul(block_size);
    let available = stat.f_bavail.saturating_mul(block_size);
    Ok((total, available))
}

#[cfg(not(unix))]
fn storage_capacity(path: &std::path::Path) -> Result<(u64, u64), String> {
    if path.exists() {
        Ok((0, 0))
    } else {
        Err(format!("storage root does not exist: {}", path.display()))
    }
}

fn stable_mount_id(path: &std::path::Path) -> String {
    let key = storage_device_id(path).unwrap_or_else(|| normalized_path_string(path));
    stable_id("mount", &key)
}

fn stable_id(prefix: &str, value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!("{prefix}-{}", hex::encode(&digest[..10]))
}

#[cfg(unix)]
fn storage_device_id(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(path)
        .ok()
        .map(|metadata| format!("dev:{}", metadata.dev()))
}

#[cfg(not(unix))]
fn storage_device_id(_path: &std::path::Path) -> Option<String> {
    None
}

fn parse_info_hash_hex(info_hash: &str) -> Result<[u8; 20], ()> {
    if info_hash.len() != 40 {
        return Err(());
    }
    let mut out = [0u8; 20];
    for (idx, chunk) in info_hash.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let hex = std::str::from_utf8(chunk).map_err(|_| ())?;
        out[idx] = u8::from_str_radix(hex, 16).map_err(|_| ())?;
    }
    Ok(out)
}

pub(crate) fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn unix_deadline_to_instant(deadline: i64, now_unix: i64, now: Instant) -> Option<Instant> {
    let delay = u64::try_from(deadline.saturating_sub(now_unix).max(0)).ok()?;
    now.checked_add(Duration::from_secs(delay))
}

fn setting_i64(conn: &Connection, key: &str) -> i64 {
    rt_db::get_setting(conn, key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0)
}

fn setting_bool(conn: &Connection, key: &str) -> bool {
    matches!(rt_db::get_setting(conn, key).as_deref(), Ok("1" | "true"))
}

fn setting_bool_with_default(conn: &Connection, key: &str, default: bool) -> bool {
    rt_db::get_setting(conn, key)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true"))
        .unwrap_or(default)
}

fn queue_setting_key(info_hash: &str) -> String {
    format!("{SETTING_QUEUE_PREFIX}{info_hash}")
}

fn normalize_relative_path(path: &str) -> CmdResult<String> {
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Err("path is empty".to_owned());
    }
    rt_path::SafeRelPath::from_components(&parts, false)
        .map(|path| path.as_display())
        .map_err(|e| e.to_string())
}

fn stable_partition_selected(
    hashes: &mut Vec<String>,
    selected: &std::collections::HashSet<&str>,
    selected_first: bool,
) {
    let mut picked = Vec::new();
    let mut rest = Vec::new();
    for hash in hashes.drain(..) {
        if selected.contains(hash.as_str()) {
            picked.push(hash);
        } else {
            rest.push(hash);
        }
    }
    if selected_first {
        picked.extend(rest);
        *hashes = picked;
    } else {
        rest.extend(picked);
        *hashes = rest;
    }
}

fn normalize_category(category: Option<String>) -> Option<String> {
    category
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_owned();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

fn normalize_tracker_urls(trackers: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for tracker in trackers {
        let tracker = tracker.trim().to_owned();
        if !tracker.is_empty() && !out.contains(&tracker) {
            out.push(tracker);
        }
    }
    out
}

fn engine_limits_from_row(row: rt_db::TorrentLimitRow) -> EngineTorrentLimits {
    EngineTorrentLimits {
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
    }
}

fn metadata_from_meta(meta: &TorrentMeta) -> EngineTorrentMetadata {
    match meta {
        TorrentMeta::V1(meta) => EngineTorrentMetadata {
            piece_length: meta.piece_length,
            piece_count: meta.pieces.len(),
            piece_hashes: meta.pieces.iter().map(hex::encode).collect(),
            piece_states: vec![EnginePieceState::Missing; meta.pieces.len()],
            is_private: meta.private,
            trackers: meta.all_trackers(),
            webseeds: meta.webseeds.clone(),
            comment: meta.comment.clone(),
            created_by: meta.created_by.clone(),
            creation_date: meta.creation_date,
            files: meta
                .files
                .iter()
                .map(|file| EngineTorrentFile {
                    index: file.index,
                    path: file.path.as_display(),
                    length: file.length,
                    priority: 1,
                    wanted: true,
                })
                .collect(),
        },
        TorrentMeta::Hybrid(meta, _) => EngineTorrentMetadata {
            piece_length: meta.piece_length,
            piece_count: meta.pieces.len(),
            piece_hashes: meta.pieces.iter().map(hex::encode).collect(),
            piece_states: vec![EnginePieceState::Missing; meta.pieces.len()],
            is_private: meta.private,
            trackers: meta.all_trackers(),
            webseeds: meta.webseeds.clone(),
            comment: meta.comment.clone(),
            created_by: meta.created_by.clone(),
            creation_date: meta.creation_date,
            files: meta
                .files
                .iter()
                .map(|file| EngineTorrentFile {
                    index: file.index,
                    path: file.path.as_display(),
                    length: file.length,
                    priority: 1,
                    wanted: true,
                })
                .collect(),
        },
        TorrentMeta::V2(meta) => {
            let piece_count = meta.total_length().div_ceil(meta.piece_length) as usize;
            EngineTorrentMetadata {
                piece_length: meta.piece_length,
                piece_count,
                piece_hashes: vec![String::new(); piece_count],
                piece_states: vec![EnginePieceState::Missing; piece_count],
                is_private: meta.private,
                trackers: v2_all_trackers(meta),
                webseeds: meta.webseeds.clone(),
                comment: meta.comment.clone(),
                created_by: meta.created_by.clone(),
                creation_date: meta.creation_date,
                files: meta
                    .files
                    .iter()
                    .map(|file| EngineTorrentFile {
                        index: file.index,
                        path: file.path.as_display(),
                        length: file.length,
                        priority: 1,
                        wanted: true,
                    })
                    .collect(),
            }
        }
    }
}

fn metadata_from_placeholder_row(row: &TorrentRow) -> EngineTorrentMetadata {
    EngineTorrentMetadata {
        piece_length: 0,
        piece_count: 0,
        piece_hashes: Vec::new(),
        piece_states: Vec::new(),
        is_private: row.is_private,
        trackers: row.trackers.clone(),
        webseeds: Vec::new(),
        comment: None,
        created_by: None,
        creation_date: None,
        files: Vec::new(),
    }
}

fn decode_info_hash_bytes(info_hash: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = hex::decode(info_hash)?;
    match bytes.len() {
        20 | 32 => Ok(bytes),
        len => anyhow::bail!("expected 20-byte or 32-byte info hash, got {len} bytes"),
    }
}

fn prune_empty_dirs(
    mut dir: Option<&std::path::Path>,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    while let Some(current) = dir {
        if current == root {
            break;
        }
        match std::fs::remove_dir(current) {
            Ok(()) => {
                dir = current.parent();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::DirectoryNotEmpty
                        | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use rt_bencode::{encode, BValue};
    use rt_hash::{merkle_root, BlockHash};
    use rt_metainfo::TorrentFileV1;
    use rt_path::SafeRelPath;

    fn test_resource_governor() -> ResourceGovernor {
        ResourceGovernor::new(ResourceGovernorConfig::default())
    }

    fn tiny_api_snapshot_governor() -> ResourceGovernor {
        let mut class_caps_bytes = [0; MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::ApiSnapshot as usize] = 4;
        ResourceGovernor::new(ResourceGovernorConfig {
            total_cap_bytes: 4,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        })
    }

    #[test]
    fn engine_handle_liveness_tracks_command_receiver() {
        let (tx, rx) = mpsc::channel(1);
        let handle = EngineHandle { tx };
        assert!(handle.is_alive());
        drop(rx);
        assert!(!handle.is_alive());
    }

    #[test]
    fn storage_io_config_maps_native_storage_toml() {
        let mut config = Config::default();
        config.storage.file_pool_size = 99;
        config.storage.idle_file_ttl_secs = 12;
        config.storage.io_worker_threads = 3;
        config.storage.io_queue_depth = 77;
        config.storage.hash_worker_threads = 4;
        config.storage.hash_queue_depth = 88;
        config.storage.preallocation_mode = rt_config::StoragePreallocationMode::Full;
        config.storage.durability_mode = rt_config::StorageDurabilityMode::Strict;
        config.storage.peer_read_readahead_bytes = 128 * 1024;
        config.storage.peer_read_cache_entries = 17;
        config.storage.peer_read_elevator_budget_ms = 9;

        let io = storage_io_config_from_config(&config);

        assert_eq!(io.file_pool_size, 99);
        assert_eq!(io.idle_file_ttl_secs, 12);
        assert_eq!(io.io_worker_threads, 3);
        assert_eq!(io.io_queue_depth, 77);
        assert_eq!(io.hash_worker_threads, 4);
        assert_eq!(io.hash_queue_depth, 88);
        assert_eq!(io.preallocation_mode, PreallocationMode::Full);
        assert_eq!(io.durability_mode, DurabilityMode::Strict);
        assert_eq!(io.peer_read_readahead_bytes, 128 * 1024);
        assert_eq!(io.peer_read_cache_entries, 17);
        assert_eq!(io.peer_read_elevator_budget_ms, 9);

        config.storage.device_elevator_enabled = false;
        assert_eq!(
            storage_io_config_from_config(&config).peer_read_elevator_budget_ms,
            0
        );
    }

    fn meta() -> TorrentMetaV1 {
        TorrentMetaV1 {
            info_hash: [1u8; 20],
            announce: Some("http://tracker.example.com/announce".into()),
            announce_list: Vec::new(),
            webseeds: Vec::new(),
            comment: None,
            created_by: None,
            creation_date: None,
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2u8; 20], [3u8; 20]],
            files: vec![TorrentFileV1 {
                index: 0,
                length: 20_000,
                path: SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
                pad: false,
            }],
            private: false,
            raw: b"torrent".to_vec(),
        }
    }

    fn raw_single_file_torrent() -> Vec<u8> {
        let pieces = vec![7u8; 20];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"restore.bin")),
            (b"piece length", BValue::Int(16_384)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    /// Like `raw_single_file_torrent`, but with a real piece hash computed
    /// from `content` (which must fit in one 16KiB piece) instead of a
    /// placeholder -- lets a test prove a recheck actually reads real bytes
    /// from a real path, rather than only checking file presence.
    fn raw_single_file_torrent_with_content(content: &[u8]) -> Vec<u8> {
        let mut hasher = Sha1::new();
        hasher.update(content);
        let piece_hash: [u8; 20] = hasher.finalize().into();
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(content.len() as i64)),
            (b"name", BValue::Bytes(b"restore.bin")),
            (b"piece length", BValue::Int(16_384)),
            (b"pieces", BValue::Bytes(&piece_hash)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    fn raw_v2_torrent() -> Vec<u8> {
        raw_v2_torrent_with_root([0xAB; 32], 65_536)
    }

    fn raw_v2_torrent_with_root(pieces_root: [u8; 32], length: i64) -> Vec<u8> {
        let leaf = BValue::Dict({
            let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
                (b"length", BValue::Int(length)),
                (b"pieces root", BValue::Bytes(&pieces_root)),
            ];
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            pairs
        });
        let file_node = BValue::Dict(vec![(b"".as_ref(), leaf)]);
        let file_tree = BValue::Dict(vec![(b"data.bin".as_ref(), file_node)]);
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"file tree", file_tree),
            (b"meta version", BValue::Int(2)),
            (b"name", BValue::Bytes(b"v2dir")),
            (b"piece length", BValue::Int(16_384)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"announce", BValue::Bytes(b"http://tracker.example/v2")),
            (b"info", BValue::Dict(info_pairs)),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    fn v2_file_root(content: &[u8]) -> [u8; 32] {
        let mut leaves = content
            .chunks(V2FileVerifier::LEAF_SIZE)
            .map(|chunk| BlockHash::of(chunk).0)
            .collect::<Vec<_>>();
        if leaves.is_empty() {
            leaves.push(BlockHash::of(&[]).0);
        }
        merkle_root(&leaves)
    }

    #[test]
    fn row_conversion_preserves_session_fields() {
        let meta = meta();
        let mut entry = TorrentEntry::new("01".repeat(20), meta.name.clone(), "/tmp/data".into());
        entry.transition(TorrentState::Downloading).unwrap();
        entry.stats.add_download(10);
        entry.stats.add_upload(5);

        let row = row_from_entry(&entry, &TorrentMeta::V1(meta));
        assert_eq!(row.info_hash, entry.info_hash);
        assert_eq!(row.state, "downloading");
        assert_eq!(row.total_length, 20_000);
        assert_eq!(row.piece_count, 2);
        assert_eq!(row.trackers, vec!["http://tracker.example.com/announce"]);

        let restored = entry_from_row(&row);
        assert_eq!(restored.info_hash, entry.info_hash);
        assert_eq!(restored.state, TorrentState::Downloading);
        assert_eq!(restored.stats.downloaded, 10);
        assert_eq!(restored.stats.uploaded, 5);
    }

    #[test]
    fn prune_empty_dirs_stops_at_root_and_keeps_nonempty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("downloads");
        let empty_leaf = root.join("torrent").join("subdir");
        std::fs::create_dir_all(&empty_leaf).unwrap();
        prune_empty_dirs(Some(&empty_leaf), &root).unwrap();
        assert!(root.exists());
        assert!(!root.join("torrent").exists());

        let nonempty = root.join("other").join("nested");
        std::fs::create_dir_all(&nonempty).unwrap();
        std::fs::write(root.join("other").join("keep.bin"), b"data").unwrap();
        prune_empty_dirs(Some(&nonempty), &root).unwrap();
        assert!(!nonempty.exists());
        assert!(root.join("other").exists());
        assert!(root.join("other").join("keep.bin").exists());
    }

    #[test]
    fn parse_info_hash_hex_rejects_invalid_input() {
        assert_eq!(parse_info_hash_hex(&"0a".repeat(20)).unwrap(), [10u8; 20]);
        assert!(parse_info_hash_hex("abc").is_err());
        assert!(parse_info_hash_hex(&"zz".repeat(20)).is_err());
    }

    #[test]
    fn decode_info_hash_bytes_accepts_v1_and_v2_lengths() {
        assert_eq!(decode_info_hash_bytes(&"0a".repeat(20)).unwrap().len(), 20);
        assert_eq!(decode_info_hash_bytes(&"0b".repeat(32)).unwrap().len(), 32);
        assert!(decode_info_hash_bytes(&"0c".repeat(21)).is_err());
    }

    #[test]
    fn metadata_projection_preserves_files_trackers_and_privacy() {
        let mut meta = meta();
        meta.private = true;
        meta.announce_list = vec![vec![
            "http://tracker.example.com/announce".into(),
            "udp://tracker.two:6969/announce".into(),
        ]];
        meta.comment = Some("Release notes".to_owned());
        meta.created_by = Some("TorrentNG fixture".to_owned());
        meta.creation_date = Some(1_700_000_000);
        meta.files = vec![TorrentFileV1 {
            index: 7,
            length: 42,
            path: SafeRelPath::from_components(&["dir", "file.bin"], false).unwrap(),
            offset: 0,
            pad: false,
        }];

        let projected = metadata_from_meta(&TorrentMeta::V1(meta));

        assert_eq!(projected.piece_length, 16_384);
        assert_eq!(projected.piece_count, 2);
        assert_eq!(
            projected.piece_hashes,
            vec![hex::encode([2u8; 20]), hex::encode([3u8; 20])]
        );
        assert!(projected.is_private);
        assert_eq!(projected.comment.as_deref(), Some("Release notes"));
        assert_eq!(projected.created_by.as_deref(), Some("TorrentNG fixture"));
        assert_eq!(projected.creation_date, Some(1_700_000_000));
        assert_eq!(
            projected.trackers,
            vec![
                "http://tracker.example.com/announce".to_owned(),
                "udp://tracker.two:6969/announce".to_owned()
            ]
        );
        assert_eq!(projected.files.len(), 1);
        assert_eq!(projected.files[0].index, 7);
        assert_eq!(projected.files[0].path, "dir/file.bin");
        assert_eq!(projected.files[0].length, 42);
    }

    #[test]
    fn torrent_blob_export_preserves_raw_metainfo_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();

        let raw = raw_single_file_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(config),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        engine.save_torrent_blob(&info_hash, &raw).unwrap();

        assert_eq!(engine.load_torrent_blob(&info_hash).unwrap(), raw);
    }

    #[test]
    fn pure_v2_metadata_projects_to_engine_and_db_shapes() {
        let raw = raw_v2_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        assert_eq!(info_hash.len(), 64);

        let entry = TorrentEntry::new(info_hash.clone(), meta.name().to_owned(), "/tmp".into());
        let row = row_from_entry(&entry, &meta);
        assert_eq!(row.info_hash, info_hash);
        assert_eq!(row.name, "v2dir");
        assert_eq!(row.total_length, 65_536);
        assert_eq!(row.piece_length, 16_384);
        assert_eq!(row.piece_count, 4);
        assert_eq!(row.trackers, vec!["http://tracker.example/v2"]);

        let files = meta_file_rows(&info_hash, &meta);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "v2dir/data.bin");
        assert_eq!(files[0].length, 65_536);

        let projected = metadata_from_meta(&meta);
        assert_eq!(projected.piece_count, 4);
        assert_eq!(projected.piece_states.len(), 4);
        assert_eq!(projected.files[0].path, "v2dir/data.bin");
    }

    #[tokio::test]
    async fn pure_v2_recheck_verifies_file_roots_without_torrent_task() {
        let temp = tempfile::tempdir().unwrap();
        let save_root = temp.path().join("downloads");
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        // TNG-001: execution now only trusts server-registered storage
        // roots, never a caller/task-local path -- register save_root the
        // same way daemon startup does, or recheck correctly fails closed.
        config.storage.download_dir = save_root.clone();
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();

        let content: Vec<u8> = (0..(V2FileVerifier::LEAF_SIZE + 11))
            .map(|idx| idx as u8)
            .collect();
        let raw = raw_v2_torrent_with_root(v2_file_root(&content), content.len() as i64);
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        std::fs::create_dir_all(save_root.join("v2dir")).unwrap();
        std::fs::write(save_root.join("v2dir").join("data.bin"), &content).unwrap();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            "v2dir".into(),
            save_root.to_string_lossy().into(),
        );
        entry.total_length = content.len() as u64;
        entry.amount_left = content.len() as u64;
        entry.state = TorrentState::Paused;
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let job_id = engine.create_recheck_job(&info_hash).unwrap();

        engine
            .recheck_pure_v2_torrent(&info_hash, Some(job_id.clone()))
            .await
            .unwrap();

        let reg = registry.read().await;
        let entry = reg.get(&info_hash).unwrap();
        assert_eq!(entry.state, TorrentState::Seeding);
        assert_eq!(entry.amount_left, 0);
        drop(reg);
        {
            let db = engine.db.lock().unwrap();
            let job = rt_db::get_job(&db, &job_id).unwrap();
            assert_eq!(job.state, JOB_STATE_COMPLETED);
            assert_eq!(job.done, 1);
            assert!(job.invalid_pieces.is_empty());
        }

        let paused_job_id = engine.create_recheck_job(&info_hash).unwrap();
        engine
            .control_recheck_job(&paused_job_id, JOB_STATE_PAUSED)
            .await
            .unwrap();
        {
            let db = engine.db.lock().unwrap();
            let paused_job = rt_db::get_job(&db, &paused_job_id).unwrap();
            assert_eq!(paused_job.state, JOB_STATE_PAUSED);
        }

        let cancelled_job_id = engine.create_recheck_job(&info_hash).unwrap();
        engine
            .control_recheck_job(&cancelled_job_id, JOB_STATE_CANCELLED)
            .await
            .unwrap();
        {
            let db = engine.db.lock().unwrap();
            let cancelled_job = rt_db::get_job(&db, &cancelled_job_id).unwrap();
            assert_eq!(cancelled_job.state, JOB_STATE_CANCELLED);
        }
    }

    #[tokio::test]
    async fn add_torrent_rolls_back_registry_row_when_blob_write_fails() {
        // TNG-008: `add_torrent` registers the torrent in the in-memory
        // registry before writing its blob to disk. If the blob write
        // fails, that registry row must not be left behind as a phantom
        // entry with nothing on disk backing it.
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        std::fs::create_dir_all(&config.storage.download_dir).unwrap();
        std::fs::create_dir_all(&config.daemon.session_dir).unwrap();
        // Block `save_torrent_blob`'s `create_dir_all(session_dir/torrents)`
        // by occupying that path with a plain file instead of a directory.
        std::fs::write(
            config.daemon.session_dir.join("torrents"),
            b"not a directory",
        )
        .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let raw = raw_single_file_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);

        let result = engine.add_torrent(meta, None, true, None, vec![]).await;

        assert!(
            result.is_err(),
            "expected the blob write failure to surface as an error"
        );
        assert!(
            registry.read().await.get(&info_hash).is_none(),
            "a torrent whose blob could not be written must not remain in the registry"
        );
    }

    #[tokio::test]
    async fn add_torrent_rolls_back_registry_and_blob_when_db_persist_fails() {
        // TNG-008: if the blob write succeeds but the DB upsert fails,
        // both the registry row *and* the now-orphaned blob file must be
        // rolled back -- otherwise a blob with nothing in the registry or
        // DB pointing at it is left behind indefinitely.
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        std::fs::create_dir_all(&config.storage.download_dir).unwrap();
        std::fs::create_dir_all(&config.daemon.session_dir).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        conn.execute_batch("PRAGMA query_only = ON;").unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(config.clone()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let raw = raw_single_file_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);

        let result = engine.add_torrent(meta, None, true, None, vec![]).await;

        assert!(
            result.is_err(),
            "expected the read-only-database persist failure to surface as an error"
        );
        assert!(
            registry.read().await.get(&info_hash).is_none(),
            "a torrent whose DB row could not be persisted must not remain in the registry"
        );
        assert!(
            !torrent_blob_path(&config, &info_hash).exists(),
            "the blob written before the DB failure must be cleaned up, not left orphaned"
        );
    }

    #[tokio::test]
    async fn add_v2_only_magnet_persists_metadata_placeholder() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        let payload = config.storage.download_dir.join("payload");
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let magnet = MagnetLink {
            info_hash_v1: None,
            info_hash_v2: Some([0x22; 32]),
            display_name: Some("v2-only".to_owned()),
            trackers: vec!["https://tracker.example/announce".to_owned()],
        };

        let hash = engine
            .add_magnet(
                magnet,
                Some(payload),
                false,
                Some("movies".to_owned()),
                vec!["v2".to_owned()],
            )
            .await
            .unwrap();

        assert_eq!(hash, hex::encode([0x22; 32]));
        assert!(engine.torrent_chans.is_empty());
        let reg = registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.name, "v2-only");
        assert_eq!(entry.state, TorrentState::MetadataPending);
        assert_eq!(entry.category.as_deref(), Some("movies"));
        assert_eq!(entry.tags, vec!["v2".to_owned()]);
        drop(reg);
        {
            let db = engine.db.lock().unwrap();
            let row = rt_db::get(&db, &hash).unwrap();
            assert_eq!(row.state, "metadata_pending");
            assert_eq!(row.trackers, vec!["https://tracker.example/announce"]);
            let trackers = rt_db::list_torrent_trackers(&db, &hash).unwrap();
            assert_eq!(trackers.len(), 1);
            assert_eq!(trackers[0].url, "https://tracker.example/announce");
            assert_eq!(trackers[0].status, "pending");
        }

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::PauseTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert_eq!(
            registry.read().await.get(&hash).unwrap().state,
            TorrentState::Paused
        );

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ResumeTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert_eq!(
            registry.read().await.get(&hash).unwrap().state,
            TorrentState::MetadataPending
        );

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReannounceTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert!(engine.torrent_chans.is_empty());
    }

    #[tokio::test]
    async fn complete_v2_only_magnet_persists_metadata_without_task() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().to_path_buf();
        config.daemon.session_dir = temp.path().join("session");
        let payload = temp.path().join("downloads/payload");
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        std::fs::create_dir_all(torrent_blob_dir(&engine.config)).unwrap();
        let raw = raw_v2_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let hash = meta_info_hash_hex(&meta);
        let magnet = MagnetLink {
            info_hash_v1: None,
            info_hash_v2: Some(match meta {
                TorrentMeta::V2(ref meta) => meta.info_hash_v2,
                _ => unreachable!(),
            }),
            display_name: Some("placeholder".to_owned()),
            trackers: vec!["https://tracker.example/announce".to_owned()],
        };

        let added = engine
            .add_magnet(
                magnet,
                Some(payload),
                false,
                Some("movies".to_owned()),
                vec!["v2".to_owned()],
            )
            .await
            .unwrap();
        assert_eq!(added, hash);

        engine.complete_magnet(&hash, raw.clone()).await.unwrap();

        assert!(engine.torrent_chans.is_empty());
        let reg = registry.read().await;
        let entry = reg.get(&hash).unwrap();
        assert_eq!(entry.name, "v2dir");
        assert_eq!(entry.total_length, 65_536);
        assert_eq!(entry.amount_left, 65_536);
        assert_eq!(entry.state, TorrentState::Paused);
        assert_eq!(entry.category.as_deref(), Some("movies"));
        assert_eq!(entry.tags, vec!["v2".to_owned()]);
        drop(reg);

        engine
            .update_torrent_labels_inner(
                &hash,
                Some(Some("archive".to_owned())),
                vec!["complete".to_owned()],
                vec!["v2".to_owned()],
            )
            .await
            .unwrap();
        engine
            .update_torrent_fields_inner(
                &hash,
                Some("v2-renamed".to_owned()),
                Some(temp.path().join("moved")),
            )
            .await
            .unwrap();
        engine
            .update_file_priorities_inner(&hash, vec![0], 0)
            .await
            .unwrap();
        engine
            .rename_file_path_inner(&hash, 0, "renamed/data.bin".to_owned())
            .await
            .unwrap();

        let projected = engine.load_torrent_metadata(&hash).unwrap();
        assert_eq!(projected.piece_count, 4);
        assert_eq!(projected.trackers, vec!["http://tracker.example/v2"]);
        assert_eq!(projected.files.len(), 1);
        assert_eq!(projected.files[0].path, "renamed/data.bin");
        assert_eq!(projected.files[0].priority, 0);
        assert!(!projected.files[0].wanted);

        // TNG-016: a taskless pure-v2 placeholder has no transfer support
        // implemented yet, and must say so explicitly rather than silently
        // accepting peers it can never actually use.
        let add_peers_result = engine
            .add_peers_inner(&hash, vec!["127.0.0.1:6881".parse::<SocketAddr>().unwrap()])
            .await;
        assert_eq!(
            add_peers_result,
            Err("pure v2 peer transfer is not implemented".to_owned())
        );
        assert!(engine.torrent_peers_inner(&hash).await.unwrap().is_empty());
        let diagnostic = engine.diagnose_torrent_inner(&hash).await.unwrap();
        assert!(diagnostic
            .reasons
            .iter()
            .any(|reason| reason.contains("pure v2 torrent has metadata")));
        assert!(diagnostic
            .next_actions
            .iter()
            .any(|action| action.contains("v2 transfer support")));

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::PauseTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert_eq!(
            registry.read().await.get(&hash).unwrap().state,
            TorrentState::Paused
        );

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ResumeTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        // TNG-016: resuming a taskless pure-v2 placeholder must say it can't
        // actually transfer, not silently report success while doing
        // nothing -- the torrent correctly stays Paused either way.
        assert_eq!(
            rx.await.unwrap(),
            Err("pure v2 peer transfer is not implemented".to_owned())
        );
        assert_eq!(
            registry.read().await.get(&hash).unwrap().state,
            TorrentState::Paused
        );

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReannounceTorrent {
                    info_hash: hash.clone(),
                    reply,
                })
                .await
        );
        // TNG-016: same honesty fix for tracker lifecycle (announce) on a
        // taskless pure-v2 placeholder.
        assert_eq!(
            rx.await.unwrap(),
            Err("pure v2 tracker lifecycle is not implemented".to_owned())
        );
        assert!(engine.torrent_chans.is_empty());

        assert_eq!(
            std::fs::read(torrent_blob_path(&engine.config, &hash)).unwrap(),
            raw
        );
        {
            let db = engine.db.lock().unwrap();
            let row = rt_db::get(&db, &hash).unwrap();
            assert_eq!(row.name, "v2-renamed");
            assert_eq!(row.state, "paused");
            assert_eq!(row.total_length, 65_536);
            assert_eq!(row.piece_length, 16_384);
            assert_eq!(row.piece_count, 4);
            assert_eq!(row.save_path, temp.path().join("moved").to_string_lossy());
            assert_eq!(row.category.as_deref(), Some("archive"));
            assert_eq!(row.tags, vec!["complete".to_owned()]);
            assert_eq!(row.trackers, vec!["http://tracker.example/v2"]);
            let files = rt_db::list_torrent_files(&db, &hash).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, "renamed/data.bin");
            assert_eq!(files[0].length, 65_536);
            assert_eq!(files[0].priority, 0);
            assert!(!files[0].wanted);
            let trackers = rt_db::list_torrent_trackers(&db, &hash).unwrap();
            assert_eq!(trackers.len(), 1);
            assert_eq!(trackers[0].url, "http://tracker.example/v2");
            assert_eq!(trackers[0].left_bytes, 65_536);
        }

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::RemoveTorrent {
                    info_hash: hash.clone(),
                    delete_files: false,
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert!(registry.read().await.get(&hash).is_none());
        assert!(std::fs::read(torrent_blob_path(&engine.config, &hash)).is_err());
        let db = engine.db.lock().unwrap();
        assert!(rt_db::get(&db, &hash).is_err());
    }

    #[test]
    fn metadata_placeholder_projection_preserves_trackers() {
        let mut row = row_from_entry(
            &TorrentEntry::new("02".repeat(20), "pending".into(), "/tmp/data".into()),
            &TorrentMeta::V1(meta()),
        );
        row.total_length = 0;
        row.piece_length = 0;
        row.piece_count = 0;
        row.trackers = vec!["udp://tracker.example:6969/announce".into()];
        row.state = "metadata_pending".into();

        let projected = metadata_from_placeholder_row(&row);

        assert_eq!(projected.piece_length, 0);
        assert_eq!(projected.piece_count, 0);
        assert!(projected.piece_hashes.is_empty());
        assert_eq!(projected.trackers, row.trackers);
        assert!(projected.files.is_empty());
    }

    #[test]
    fn label_normalization_trims_dedupes_and_drops_empty_values() {
        assert_eq!(
            normalize_category(Some(" movies ".into())).as_deref(),
            Some("movies")
        );
        assert_eq!(normalize_category(Some("   ".into())), None);
        assert_eq!(
            normalize_tags(vec![
                " hd ".into(),
                String::new(),
                "hd".into(),
                "archive".into()
            ]),
            vec!["hd".to_owned(), "archive".to_owned()]
        );
    }

    #[test]
    fn append_session_event_persists_payload() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.append_session_event(
            Some(&"a".repeat(40)),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({"paused": false}),
        );

        let db = engine.db.lock().unwrap();
        let events = rt_db::list_session_events(&db, Some(&"a".repeat(40)), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EVENT_TORRENT_ADDED);
        assert_eq!(events[0].message.as_deref(), Some("torrent added"));
        assert!(events[0].payload.contains("\"paused\":false"));
    }

    #[test]
    fn recheck_job_helpers_persist_state_and_events() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let job_id = engine.create_recheck_job(&"b".repeat(40)).unwrap();
        engine.update_job_state(&job_id, JOB_STATE_RUNNING, None, Some("recheck dispatched"));

        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.kind, JOB_KIND_RECHECK);
        assert_eq!(job.state, JOB_STATE_RUNNING);
        assert_eq!(job.affected_torrents, vec!["b".repeat(40)]);
        assert!(job.started_at.is_some());
        let events = rt_db::list_job_events(&db, &job_id, 10).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "check_started");
        assert_eq!(events[1].kind, "job_running");
        assert_eq!(events[2].kind, "job_queued");
    }

    #[tokio::test]
    async fn recheck_job_control_sends_torrent_commands_and_updates_state() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(4);
        let info_hash = "c".repeat(40);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let job_id = engine.create_recheck_job(&info_hash).unwrap();
        engine
            .control_recheck_job(&job_id, JOB_STATE_PAUSED)
            .await
            .unwrap();
        assert!(matches!(torrent_rx.recv().await, Some(TorrentCmd::Pause)));

        engine
            .control_recheck_job(&job_id, JOB_STATE_RUNNING)
            .await
            .unwrap();
        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::Recheck { job_id: Some(id) }) if id == job_id
        ));

        engine
            .control_recheck_job(&job_id, JOB_STATE_CANCELLED)
            .await
            .unwrap();
        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::CancelJob { job_id: id }) if id == job_id
        ));
        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.state, JOB_STATE_CANCELLED);
        assert!(job.finished_at.is_some());
    }

    #[tokio::test]
    async fn add_peers_forwards_external_peers_to_torrent_task() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(4);
        let info_hash = "d".repeat(40);
        let peer = "127.0.0.1:6881".parse::<SocketAddr>().unwrap();
        let mut registry = SessionRegistry::new();
        registry
            .add(TorrentEntry::new(
                info_hash.clone(),
                "delta".to_owned(),
                "/tmp".to_owned(),
            ))
            .unwrap();
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(registry)),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .add_peers_inner(&info_hash, vec![peer])
            .await
            .unwrap();
        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::PriorityPeers(peers)) if peers == vec![peer]
        ));
    }

    #[test]
    fn global_limits_persist_to_settings_table() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        assert_eq!(
            engine.global_limits_inner().unwrap(),
            EngineGlobalLimits::default()
        );
        engine
            .update_global_limits_inner(EngineGlobalLimits {
                download_limit: 123,
                upload_limit: 456,
                speed_limits_mode: true,
            })
            .unwrap();
        assert_eq!(
            engine.global_limits_inner().unwrap(),
            EngineGlobalLimits {
                download_limit: 123,
                upload_limit: 456,
                speed_limits_mode: true,
            }
        );
    }

    #[tokio::test]
    async fn network_features_persist_and_notify_running_torrents() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(4);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert("a".repeat(40), torrent_tx);
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_network_features_inner(EngineNetworkFeatures {
                dht: false,
                pex: false,
            })
            .await
            .unwrap();
        assert_eq!(
            engine.network_features_inner().unwrap(),
            EngineNetworkFeatures {
                dht: false,
                pex: false,
            }
        );
        match torrent_rx.recv().await {
            Some(TorrentCmd::UpdatePeerExchange(false)) => {}
            other => panic!("expected PEX update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queue_order_moves_are_persisted() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let mut registry = SessionRegistry::new();
        for suffix in ["a", "b", "c"] {
            registry
                .add(TorrentEntry::new(
                    suffix.repeat(40),
                    suffix.to_owned(),
                    "/tmp".to_owned(),
                ))
                .unwrap();
        }
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(registry)),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_queue_order_inner(vec!["c".repeat(40)], QueueMove::Top)
            .await
            .unwrap();
        assert_eq!(engine.queue_priority_inner(&"c".repeat(40)).unwrap(), 0);
        assert_eq!(engine.queue_priority_inner(&"a".repeat(40)).unwrap(), 1);
        assert_eq!(engine.queue_priority_inner(&"b".repeat(40)).unwrap(), 2);

        engine
            .update_queue_order_inner(vec!["c".repeat(40)], QueueMove::Down)
            .await
            .unwrap();
        assert_eq!(engine.queue_priority_inner(&"a".repeat(40)).unwrap(), 0);
        assert_eq!(engine.queue_priority_inner(&"c".repeat(40)).unwrap(), 1);
    }

    #[tokio::test]
    async fn rename_file_and_folder_paths_update_metadata_projection() {
        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let info_hash = "9".repeat(40);
        let mut registry = SessionRegistry::new();
        let entry = TorrentEntry::new(info_hash.clone(), "paths".to_owned(), "/tmp".to_owned());
        rt_db::upsert(&conn, &row_from_entry(&entry, &TorrentMeta::V1(meta()))).unwrap();
        registry.add(entry).unwrap();
        rt_db::replace_torrent_files(
            &mut conn,
            &info_hash,
            &[
                rt_db::TorrentFileRow {
                    info_hash: info_hash.clone(),
                    file_index: 0,
                    path: "old/a.bin".to_owned(),
                    length: 1,
                    offset: 0,
                    priority: 1,
                    wanted: true,
                    completed_bytes: 0,
                },
                rt_db::TorrentFileRow {
                    info_hash: info_hash.clone(),
                    file_index: 1,
                    path: "old/b.bin".to_owned(),
                    length: 1,
                    offset: 1,
                    priority: 1,
                    wanted: true,
                    completed_bytes: 0,
                },
            ],
        )
        .unwrap();
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(registry)),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .rename_file_path_inner(&info_hash, 1, "old/c.bin".to_owned())
            .await
            .unwrap();
        engine
            .rename_folder_path_inner(&info_hash, "old".to_owned(), "new".to_owned())
            .await
            .unwrap();
        let db = engine.db.lock().unwrap();
        let files = rt_db::list_torrent_files(&db, &info_hash).unwrap();
        assert_eq!(files[0].path, "new/a.bin");
        assert_eq!(files[1].path, "new/c.bin");
    }

    #[tokio::test]
    async fn update_torrent_trackers_persists_summary_and_detail_rows() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let info_hash = "f".repeat(40);
        let mut entry = TorrentEntry::new(info_hash.clone(), "tracked".into(), "/data".into());
        entry.total_length = 1_000;
        entry.stats.downloaded = 250;
        let row = row_from_entry(&entry, &TorrentMeta::V1(meta()));
        rt_db::upsert(&conn, &row).unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if let Some(TorrentCmd::GetRuntimeStats { reply }) = torrent_rx.recv().await {
                let _ = reply.send(crate::command::TorrentRuntimeStats {
                    connected_peers: 1,
                    outstanding_requests: 2,
                    fastresume_dirty_pieces: 3,
                    completed_piece_verify_from_memory: 4,
                    completed_piece_verify_from_disk: 5,
                    piece_assembly_buffers: 2,
                    piece_assembly_bytes: 4096,
                    piece_assembly_evictions: 1,
                    peer_request_window_reductions: 6,
                    peer_rx_buffer_bytes: 7,
                    peer_tx_buffer_bytes: 8,
                    peer_command_queue_depth: 11,
                    peer_command_queue_capacity: 12,
                    peer_command_queue_full: 13,
                    peer_command_queue_bytes: 11 * 128,
                    tracker_peer_cache_entries: 9,
                    tracker_peer_cache_drops: 10,
                    tracker_peer_cache_bytes: 576,
                    ..Default::default()
                });
            }
        });
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert("e".repeat(40), torrent_tx);

        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_torrent_trackers_inner(
                &info_hash,
                vec![
                    " udp://tracker.one/announce ".into(),
                    "udp://tracker.one/announce".into(),
                    "https://tracker.two/announce".into(),
                ],
            )
            .await
            .unwrap();

        let db = engine.db.lock().unwrap();
        let row = rt_db::get(&db, &info_hash).unwrap();
        assert_eq!(
            row.trackers,
            vec![
                "udp://tracker.one/announce".to_owned(),
                "https://tracker.two/announce".to_owned()
            ]
        );
        let trackers = rt_db::list_torrent_trackers(&db, &info_hash).unwrap();
        assert_eq!(trackers.len(), 2);
        assert_eq!(trackers[0].status, "pending");
        assert_eq!(trackers[0].left_bytes, 19_750);
    }

    #[tokio::test]
    async fn update_torrent_limits_persists_and_reads_back() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let info_hash = "1".repeat(40);
        let entry = TorrentEntry::new(info_hash.clone(), "limited".into(), "/data".into());
        let row = row_from_entry(&entry, &TorrentMeta::V1(meta()));
        rt_db::upsert(&conn, &row).unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (runtime_tx, mut runtime_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if let Some(TorrentCmd::GetRuntimeStats { reply }) = runtime_rx.recv().await {
                let _ = reply.send(crate::command::TorrentRuntimeStats {
                    fastresume_dirty_pieces: 3,
                    completed_piece_verify_from_memory: 4,
                    completed_piece_verify_from_disk: 5,
                    piece_assembly_buffers: 2,
                    piece_assembly_bytes: 4096,
                    piece_assembly_evictions: 1,
                    peer_request_window_reductions: 6,
                    peer_rx_buffer_bytes: 7,
                    peer_tx_buffer_bytes: 8,
                    peer_command_queue_depth: 11,
                    peer_command_queue_capacity: 12,
                    peer_command_queue_full: 13,
                    peer_command_queue_bytes: 11 * 128,
                    tracker_peer_cache_entries: 9,
                    tracker_peer_cache_drops: 10,
                    tracker_peer_cache_bytes: 576,
                    ..Default::default()
                });
            }
        });
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert("e".repeat(40), runtime_tx);
        tokio::task::yield_now().await;

        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_torrent_limits_inner(
                &info_hash,
                EngineTorrentLimits {
                    download_limit: Some(1000),
                    upload_limit: Some(2000),
                    seed_ratio_limit: Some(1.5),
                    sequential_download: true,
                    sequential_download_from_piece: Some(7),
                    force_start: true,
                    auto_tmm: true,
                    auto_management: true,
                    ..EngineTorrentLimits::default()
                },
            )
            .await
            .unwrap();

        let limits = engine.torrent_limits_inner(&info_hash).await.unwrap();
        assert_eq!(limits.download_limit, Some(1000));
        assert_eq!(limits.upload_limit, Some(2000));
        assert_eq!(limits.seed_ratio_limit, Some(1.5));
        assert!(limits.sequential_download);
        assert_eq!(limits.sequential_download_from_piece, Some(7));
        assert!(limits.force_start);
        assert!(limits.auto_tmm);
        assert!(limits.auto_management);
    }

    #[tokio::test]
    async fn update_torrent_limits_notifies_running_torrent_task() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let info_hash = "f".repeat(40);
        let entry = TorrentEntry::new(info_hash.clone(), "running".into(), "/data".into());
        let row = row_from_entry(&entry, &TorrentMeta::V1(meta()));
        rt_db::upsert(&conn, &row).unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(1);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_torrent_limits_inner(
                &info_hash,
                EngineTorrentLimits {
                    sequential_download: true,
                    sequential_download_from_piece: Some(5),
                    ..EngineTorrentLimits::default()
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::UpdateLimits(limits))
                if limits.sequential_download && limits.sequential_download_from_piece == Some(5)
        ));
    }

    #[tokio::test]
    async fn shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(1);
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let info_hash = "f".repeat(40);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let mut torrent_tasks = HashMap::new();
        torrent_tasks.insert(
            info_hash,
            tokio::spawn(async move {
                if matches!(torrent_rx.recv().await, Some(TorrentCmd::Shutdown)) {
                    let _ = seen_tx.send(());
                }
            }),
        );
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks,
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.shutdown_torrent_tasks().await;

        assert!(seen_rx.await.is_ok());
        assert!(engine.torrent_chans.is_empty());
        assert!(engine.torrent_tasks.is_empty());
    }

    #[test]
    fn recover_interrupted_jobs_pauses_running_work() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let mut job = rt_db::JobRow {
            job_id: "job-running".to_owned(),
            kind: JOB_KIND_RECHECK.to_owned(),
            state: JOB_STATE_RUNNING.to_owned(),
            dry_run: false,
            affected_torrents: vec!["d".repeat(40)],
            total: 10,
            done: 4,
            checkpoint: 4,
            file_index: Some(0),
            piece_index: Some(4),
            byte_offset: Some(4096),
            verified_bytes: 4096,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: 1,
            started_at: Some(2),
            updated_at: 3,
            finished_at: None,
        };
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert_job(&db, &job).unwrap();
        }
        job.job_id = "job-queued".to_owned();
        job.state = JOB_STATE_QUEUED.to_owned();
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert_job(&db, &job).unwrap();
        }

        engine.recover_interrupted_jobs().unwrap();

        let db = engine.db.lock().unwrap();
        let recovered = rt_db::get_job(&db, "job-running").unwrap();
        assert_eq!(recovered.state, JOB_STATE_PAUSED);
        assert_eq!(recovered.done, 4);
        assert_eq!(recovered.checkpoint, 4);
        assert_eq!(recovered.file_index, Some(0));
        assert_eq!(recovered.piece_index, Some(4));
        assert_eq!(recovered.byte_offset, Some(4096));
        assert_eq!(recovered.verified_bytes, 4096);
        assert!(recovered.finished_at.is_none());
        let queued = rt_db::get_job(&db, "job-queued").unwrap();
        assert_eq!(queued.state, JOB_STATE_QUEUED);
        let events = rt_db::list_job_events(&db, "job-running", 10).unwrap();
        assert_eq!(events[0].kind, "job_recovered");
    }

    #[test]
    fn storage_plan_jobs_checkpoint_completed_steps() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::CopyVerifyRename,
                    source: Some(PathBuf::from("/mnt/a/source")),
                    destination: Some(PathBuf::from("/mnt/b/.target.tng-copy")),
                    bytes: 128,
                },
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::Rename,
                    source: Some(PathBuf::from("/mnt/b/.target.tng-copy")),
                    destination: Some(PathBuf::from("/mnt/b/target")),
                    bytes: 128,
                },
            ],
            rollback_steps: Vec::new(),
        };

        let job_id = engine
            .create_storage_plan_job("move", vec!["a".repeat(40)], &plan)
            .unwrap();
        engine.update_job_state(
            &job_id,
            JOB_STATE_RUNNING,
            None,
            Some("storage plan execution started"),
        );
        engine
            .persist_storage_plan_checkpoint(&job_id, "move", &plan, &[0])
            .unwrap();

        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.kind, JOB_KIND_STORAGE_PLAN);
        assert_eq!(job.state, JOB_STATE_RUNNING);
        assert_eq!(job.done, 1);
        assert_eq!(job.checkpoint, 1);
        assert_eq!(job.file_index, Some(1));
        assert_eq!(job.byte_offset, Some(128));
        drop(db);

        assert_eq!(engine.completed_storage_plan_steps(&job_id), vec![0]);

        engine
            .persist_storage_plan_checkpoint(&job_id, "move", &plan, &[0, 1])
            .unwrap();
        engine
            .persist_storage_plan_terminal(
                &job_id,
                "move",
                &plan,
                &[0, 1],
                JOB_STATE_COMPLETED,
                None,
            )
            .unwrap();

        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.state, JOB_STATE_COMPLETED);
        assert_eq!(job.done, 2);
        assert_eq!(job.checkpoint, 2);
        assert_eq!(job.file_index, Some(2));
        let events = rt_db::list_job_events(&db, &job_id, 10).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "storage_plan_checkpoint"
                && event.payload.contains("CopyVerifyRename")));
        assert_eq!(events[0].kind, "storage_plan_completed");
    }

    #[test]
    fn storage_plan_resume_steps_are_sorted_unique_and_bounded() {
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::CopyVerifyRename,
                    source: Some(PathBuf::from("/mnt/a/source")),
                    destination: Some(PathBuf::from("/mnt/b/.target.tng-copy")),
                    bytes: 128,
                },
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::Rename,
                    source: Some(PathBuf::from("/mnt/b/.target.tng-copy")),
                    destination: Some(PathBuf::from("/mnt/b/target")),
                    bytes: 128,
                },
            ],
            rollback_steps: Vec::new(),
        };

        assert_eq!(
            normalize_storage_plan_completed_steps(&plan, vec![1, 0, 1]).unwrap(),
            vec![0, 1]
        );
        assert!(normalize_storage_plan_completed_steps(&plan, vec![2]).is_err());
        assert_eq!(
            recovered_storage_plan_steps(&plan, 1, vec![1]).unwrap(),
            vec![1],
            "restart must preserve sparse completed-step indexes"
        );
        assert_eq!(
            recovered_storage_plan_steps(&plan, 2, Vec::new()).unwrap(),
            vec![0, 1],
            "legacy count fallback remains a prefix only when the event is absent"
        );
    }

    #[tokio::test]
    async fn recovered_storage_plan_reconciles_filesystem_ahead_of_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("destination.bin");
        std::fs::write(&destination, b"payload").unwrap();

        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        config.storage.download_dir = root.clone();
        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: rt_storage::PlannedStorageAction::Rename,
                source: Some(source),
                destination: Some(destination.clone()),
                bytes: 7,
            }],
            rollback_steps: Vec::new(),
        };
        let job_id = "recovery-filesystem-ahead";
        let now = unix_now_i64();
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: job_id.to_owned(),
                kind: JOB_KIND_STORAGE_PLAN.to_owned(),
                state: JOB_STATE_QUEUED.to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 1,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        rt_db::append_job_event(
            &conn,
            &rt_db::JobEventRow {
                event_id: None,
                job_id: job_id.to_owned(),
                occurred_at: now,
                kind: "storage_plan_queued".to_owned(),
                message: Some("move storage plan queued".to_owned()),
                payload: storage_plan_payload("move", &plan, &[]).to_string(),
            },
        )
        .unwrap();

        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let db = Arc::new(Mutex::new(conn));
        let storage_jobs = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 2);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db,
            cmd_rx,
            cmd_tx,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs,
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.resume_recovered_storage_jobs().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = {
                    let db = engine.db.lock().unwrap();
                    rt_db::get_job(&db, job_id).unwrap().state
                };
                if state == JOB_STATE_COMPLETED {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recovered storage job did not complete");

        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, job_id).unwrap();
        assert_eq!(job.done, 1);
        assert_eq!(job.checkpoint, 1);
        assert_eq!(std::fs::read(destination).unwrap(), b"payload");
    }

    #[test]
    fn storage_plan_execution_uses_persisted_roots_and_fails_closed() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("source.bin");
        let destination = allowed.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        rt_db::upsert_storage_root(
            &conn,
            &rt_db::StorageRootRow {
                root_id: "allowed".to_owned(),
                path: allowed.path().to_string_lossy().into_owned(),
                profile: "test".to_owned(),
                created_at: 1,
            },
        )
        .unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let plan = rt_storage::plan_move(&rt_storage::MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 7,
            available_bytes: None,
            dry_run: false,
        });

        let error = engine
            .execute_storage_plan_job("move", Vec::new(), &plan, Vec::new())
            .unwrap_err();
        assert!(error.contains("outside configured storage roots"));
        assert!(source.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn storage_plan_execution_rejects_missing_server_roots() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let error = engine.configured_storage_roots_for_execution().unwrap_err();
        assert!(error.contains("no configured storage roots"));
    }

    #[tokio::test]
    async fn update_save_path_moves_existing_payload_through_storage_plan() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        config.storage.download_dir = temp.path().to_path_buf();
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let raw = raw_single_file_torrent();
        let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected v1 torrent");
        };
        let info_hash = meta
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();
        let source_root = temp.path().join("old");
        let destination_root = temp.path().join("new");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(source_root.join("restore.bin"), vec![1u8; 1024]).unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            source_root.to_string_lossy().into(),
        );
        entry.total_length = meta.total_length();
        entry.amount_left = 0;
        entry.state = TorrentState::Paused;
        rt_db::upsert(&conn, &row_from_entry(&entry, &TorrentMeta::V1(meta))).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let db = Arc::new(Mutex::new(conn));
        let storage_jobs = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 4);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db,
            cmd_rx,
            cmd_tx,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs,
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine
            .update_torrent_fields_inner(&info_hash, None, Some(destination_root.clone()))
            .await
            .unwrap();

        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(5), engine.cmd_rx.recv())
                .await
                .expect("storage move completion was not delivered")
                .expect("engine command channel closed");
        assert!(engine.handle_cmd(completion).await);

        assert!(!source_root.join("restore.bin").exists());
        assert_eq!(
            std::fs::read(destination_root.join("restore.bin")).unwrap(),
            vec![1u8; 1024]
        );
        {
            let db = engine.db.lock().unwrap();
            let row = rt_db::get(&db, &info_hash).unwrap();
            assert_eq!(
                row.save_path,
                destination_root.to_string_lossy().to_string()
            );
            let jobs = rt_db::list_active_jobs(&db).unwrap();
            assert!(jobs.is_empty());
        }
        let reg = registry.read().await;
        let entry = reg.get(&info_hash).unwrap();
        assert_eq!(
            entry.save_path,
            destination_root.to_string_lossy().to_string()
        );
    }

    #[tokio::test]
    async fn update_save_path_reroutes_running_task_and_recheck_finds_new_root() {
        // TNG-002: a running TorrentTask caches its save_root at spawn time.
        // Before this fix, moving a torrent's files while its task was
        // still running left that cached field stale -- the task kept
        // reading/writing the OLD path forever after a move, even though
        // the files (and the DB's own save_path) had already moved. Prove
        // the fix by giving a *running* task's torrent a real, correctly
        // hashed payload only reachable via the new path after the move,
        // and checking that a post-move recheck actually finds it there.
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        config.storage.download_dir = temp.path().to_path_buf();
        // This test targets the live-task storage-move protocol; disable
        // dormant-tier restore so the fixture deliberately starts hot.
        config.runtime.torrent_tiers_enabled = false;
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let content = vec![9u8; 1024];
        let raw = raw_single_file_torrent_with_content(&content);
        let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected v1 torrent");
        };
        let info_hash = meta
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let source_root = temp.path().join("old");
        let destination_root = temp.path().join("new");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(source_root.join("restore.bin"), &content).unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: meta.name.clone(),
                total_length: meta.total_length() as i64,
                piece_length: meta.piece_length as i64,
                piece_count: meta.pieces.len() as i64,
                is_private: false,
                save_path: source_root.to_string_lossy().to_string(),
                category: None,
                tags: vec![],
                // Not paused: the task starts genuinely active (it will
                // run its own startup recheck against the real content at
                // source_root and reach Seeding before the move even
                // starts), so quiescing it for the move exercises a real
                // running task, not a dormant one -- and because it was
                // not paused beforehand, `was_paused` comes back false and
                // the post-move resume triggers its own recheck too.
                state: "seeding".to_owned(),
                added_at: 10,
                completed_at: None,
                uploaded: 0,
                downloaded: 0,
                ratio: 0.0,
                trackers: meta.all_trackers(),
            },
        )
        .unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let db = Arc::new(Mutex::new(conn));
        let storage_jobs = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 4);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db,
            cmd_rx,
            cmd_tx,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs,
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();
        assert!(
            engine.torrent_chans.contains_key(&info_hash),
            "a live task must be running for this test to exercise the quiesce/resume protocol"
        );

        engine
            .update_torrent_fields_inner(&info_hash, None, Some(destination_root.clone()))
            .await
            .unwrap();

        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(5), engine.cmd_rx.recv())
                .await
                .expect("storage move completion was not delivered")
                .expect("engine command channel closed");
        assert!(engine.handle_cmd(completion).await);

        assert!(!source_root.join("restore.bin").exists());
        assert_eq!(
            std::fs::read(destination_root.join("restore.bin")).unwrap(),
            content
        );

        // `ResumeAfterStorageMove` (which the move above sent as its last
        // step) is fire-and-forget from the engine's perspective, and it
        // triggers a recheck inside the task before that recheck's result
        // is visible here. In this test's flow, `Paused` and `Checking`
        // are always transient (quiescing sets Paused, the resume's own
        // recheck sets Checking while it runs) -- the only states a
        // successful post-move recheck can settle on are `Seeding`
        // (complete) or `Downloading` (found something invalid/missing).
        // Poll the shared registry, the same way a real client would,
        // ignoring the known-transient states rather than guessing at a
        // fixed delay.
        let final_state = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = registry.read().await.get(&info_hash).unwrap().state;
                if !matches!(state, TorrentState::Paused | TorrentState::Checking) {
                    return state;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("recheck after the move did not settle within the timeout");

        assert_eq!(
            final_state,
            TorrentState::Seeding,
            "recheck after the move should find the real content at the NEW save_root and mark \
             the torrent complete; if the running task's cached save_root had not been updated, \
             the file would be missing at the (now-empty) old path and this would stay incomplete"
        );

        if let Some(tx) = engine.torrent_chans.remove(&info_hash) {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
    }

    #[tokio::test]
    async fn load_persisted_torrents_restores_paused_registry_as_dormant() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let raw = raw_single_file_torrent();
        let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected v1 torrent");
        };
        let info_hash = meta
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: meta.name.clone(),
                total_length: meta.total_length() as i64,
                piece_length: meta.piece_length as i64,
                piece_count: meta.pieces.len() as i64,
                is_private: false,
                save_path: temp.path().join("downloads").to_string_lossy().to_string(),
                category: Some("movies".to_owned()),
                tags: vec!["restored".to_owned()],
                state: "paused".to_owned(),
                added_at: 10,
                completed_at: None,
                uploaded: 5,
                downloaded: 7,
                ratio: 0.0,
                trackers: meta.all_trackers(),
            },
        )
        .unwrap();

        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(
            !engine.torrent_chans.contains_key(&info_hash),
            "paused restore should not allocate a torrent task"
        );
        let reg = registry.read().await;
        let restored = reg.get(&info_hash).unwrap();
        assert_eq!(restored.state, TorrentState::Paused);
        assert_eq!(restored.category.as_deref(), Some("movies"));
        assert_eq!(restored.tags, vec!["restored".to_owned()]);
        drop(reg);
        {
            let db = engine.db.lock().unwrap();
            let trackers = rt_db::list_torrent_trackers(&db, &info_hash).unwrap();
            assert_eq!(trackers.len(), 1);
            assert_eq!(trackers[0].url, "http://tracker.example.com/announce");
        }
    }

    #[tokio::test]
    async fn load_persisted_torrents_restores_seeding_rows_as_dormant() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let mut conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let raw = raw_single_file_torrent();
        let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected v1 torrent");
        };
        let info_hash = meta
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: meta.name.clone(),
                total_length: meta.total_length() as i64,
                piece_length: meta.piece_length as i64,
                piece_count: meta.pieces.len() as i64,
                is_private: false,
                save_path: temp.path().join("downloads").to_string_lossy().to_string(),
                category: None,
                tags: vec![],
                // A persisted seed is retained in the registry, but its
                // peer/runtime task is reconstructed only on demand.
                state: "seeding".to_owned(),
                added_at: 10,
                completed_at: Some(20),
                uploaded: 5,
                downloaded: 1024,
                ratio: 0.5,
                trackers: meta.all_trackers(),
            },
        )
        .unwrap();

        let mut tracker_rows = tracker_rows_from_urls(
            &info_hash,
            &meta.all_trackers(),
            5,
            1024,
            meta.total_length() as i64 - 1024,
        );
        tracker_rows[0].next_announce_at = Some(unix_now_i64().saturating_sub(60));
        rt_db::replace_torrent_trackers(&mut conn, &info_hash, &tracker_rows).unwrap();

        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(
            !engine.torrent_chans.contains_key(&info_hash),
            "idle seed restore should retain only the dormant representation"
        );
        let reg = registry.read().await;
        let restored = reg.get(&info_hash).unwrap();
        assert_eq!(restored.state, TorrentState::Seeding);
        drop(reg);
        assert!(
            engine.tier_controller.next_tracker_deadline().is_some(),
            "dormant seeding restore must schedule its persisted tracker deadline"
        );
        assert!(
            engine
                .tier_controller
                .dormant_snapshot(&info_hash)
                .is_some(),
            "dormant seeding restore must retain the compact runtime projection"
        );
        assert!(engine.tier_controller.dormant_heap_bytes() > 0);
    }

    #[tokio::test]
    async fn load_persisted_v2_rows_restore_taskless_registry_and_trackers() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let raw = raw_v2_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: meta.name().to_owned(),
                total_length: meta_total_length(&meta) as i64,
                piece_length: meta_piece_length(&meta) as i64,
                piece_count: meta_piece_count(&meta) as i64,
                is_private: false,
                save_path: temp.path().join("downloads").to_string_lossy().to_string(),
                category: None,
                tags: vec!["v2".to_owned()],
                state: "paused".to_owned(),
                added_at: 10,
                completed_at: None,
                uploaded: 0,
                downloaded: 0,
                ratio: 0.0,
                trackers: meta_all_trackers(&meta),
            },
        )
        .unwrap();
        let placeholder_hash = "44".repeat(32);
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: placeholder_hash.clone(),
                name: "pending-v2".to_owned(),
                total_length: 0,
                piece_length: 0,
                piece_count: 0,
                is_private: false,
                save_path: config
                    .storage
                    .download_dir
                    .join("pending")
                    .to_string_lossy()
                    .to_string(),
                category: None,
                tags: Vec::new(),
                state: "metadata_pending".to_owned(),
                added_at: 11,
                completed_at: None,
                uploaded: 0,
                downloaded: 0,
                ratio: 0.0,
                trackers: vec!["https://tracker.example/v2-placeholder".to_owned()],
            },
        )
        .unwrap();

        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(engine.torrent_chans.is_empty());
        let reg = registry.read().await;
        assert_eq!(reg.get(&info_hash).unwrap().state, TorrentState::Paused);
        assert_eq!(
            reg.get(&placeholder_hash).unwrap().state,
            TorrentState::MetadataPending
        );
        drop(reg);
        let db = engine.db.lock().unwrap();
        let trackers = rt_db::list_torrent_trackers(&db, &info_hash).unwrap();
        assert_eq!(trackers[0].url, "http://tracker.example/v2");
        let placeholder_trackers = rt_db::list_torrent_trackers(&db, &placeholder_hash).unwrap();
        assert_eq!(
            placeholder_trackers[0].url,
            "https://tracker.example/v2-placeholder"
        );
        let events = rt_db::list_session_events(&db, Some(&placeholder_hash), 10).unwrap();
        assert!(events
            .iter()
            .any(|event| event.payload.contains("\"v2_only\":true")));
    }

    #[test]
    fn register_configured_storage_persists_root_and_mount() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();

        let roots = rt_db::list_storage_roots(&conn).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].profile, "auto");
        assert!(roots[0].path.ends_with("downloads"));
        let mounts = rt_db::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].queue_depth, 1);
        assert_eq!(mounts[0].read_concurrency, 1);
        assert_eq!(mounts[0].write_concurrency, 1);
    }

    #[test]
    fn storage_root_projection_reports_capacity_and_root_errors() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        rt_db::upsert_storage_root(
            &conn,
            &rt_db::StorageRootRow {
                root_id: "missing".to_owned(),
                path: temp.path().join("missing").to_string_lossy().to_string(),
                profile: "auto".to_owned(),
                created_at: 1,
            },
        )
        .unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(config),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let roots = engine.list_storage_roots_inner().unwrap();
        assert_eq!(roots.len(), 2);
        let ok = roots.iter().find(|root| root.ok).unwrap();
        assert!(ok.total_bytes > 0);
        assert!(ok.available_bytes <= ok.total_bytes);
        assert_eq!(
            ok.used_bytes,
            ok.total_bytes.saturating_sub(ok.available_bytes)
        );
        let missing = roots.iter().find(|root| !root.ok).unwrap();
        assert_eq!(missing.total_bytes, 0);
        assert!(missing
            .error
            .as_deref()
            .is_some_and(|error| error.contains("statvfs")));
    }

    #[tokio::test]
    async fn engine_stats_include_registry_jobs_and_trackers() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("e".repeat(40), "stats.bin".into(), "/tmp".into());
            entry.stats.add_upload(10);
            entry.stats.add_download(20);
            entry.amount_left = 30;
            let _ = entry.transition(TorrentState::Downloading);
            reg.add(entry).unwrap();
        }
        let info_hash = "e".repeat(40);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if let Some(TorrentCmd::GetRuntimeStats { reply }) = torrent_rx.recv().await {
                let _ = reply.send(crate::command::TorrentRuntimeStats {
                    fastresume_dirty_pieces: 3,
                    completed_piece_verify_from_memory: 4,
                    completed_piece_verify_from_disk: 5,
                    piece_assembly_buffers: 2,
                    piece_assembly_bytes: 4096,
                    piece_assembly_evictions: 1,
                    peer_request_window_reductions: 6,
                    peer_rx_buffer_bytes: 7,
                    peer_tx_buffer_bytes: 8,
                    peer_command_queue_depth: 11,
                    peer_command_queue_capacity: 12,
                    peer_command_queue_full: 13,
                    peer_command_queue_bytes: 11 * 128,
                    tracker_peer_cache_entries: 9,
                    tracker_peer_cache_drops: 10,
                    tracker_peer_cache_bytes: 576,
                    ..Default::default()
                });
            }
        });
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let (dht_tx, mut dht_rx) = mpsc::channel(1);
        tokio::spawn(async move {
            if let Some(DhtCommand::GetStats { reply }) = dht_rx.recv().await {
                let _ = reply.send(crate::dht_task::DhtRuntimeStats {
                    routing_nodes: 11,
                    announced_peer_sets: 12,
                    announced_peers: 13,
                    tracked_torrents: 14,
                    outstanding_requests: 15,
                    queried_nodes: 16,
                });
            }
        });
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: Some(dht_tx),
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        let job_id = engine.create_recheck_job(&info_hash).unwrap();
        engine.update_job_state(&job_id, JOB_STATE_RUNNING, None, Some("running"));
        {
            let mut db = engine.db.lock().unwrap();
            rt_db::upsert(
                &db,
                &TorrentRow {
                    info_hash: info_hash.clone(),
                    name: "stats.bin".to_owned(),
                    total_length: 100,
                    piece_length: 10,
                    piece_count: 10,
                    is_private: false,
                    save_path: "/tmp".to_owned(),
                    category: None,
                    tags: Vec::new(),
                    state: "downloading".to_owned(),
                    added_at: 1,
                    completed_at: None,
                    uploaded: 10,
                    downloaded: 20,
                    ratio: 0.5,
                    trackers: Vec::new(),
                },
            )
            .unwrap();
            rt_db::replace_torrent_trackers(
                &mut db,
                &info_hash,
                &[rt_db::TorrentTrackerRow {
                    info_hash: info_hash.clone(),
                    tracker_index: 0,
                    tier: 0,
                    url: "http://tracker/announce".to_owned(),
                    status: "error".to_owned(),
                    last_announce_at: None,
                    next_announce_at: None,
                    last_success_at: None,
                    failure_reason: Some("timeout".to_owned()),
                    warning_message: None,
                    seeders: None,
                    leechers: None,
                    completed: None,
                    uploaded: 10,
                    downloaded: 20,
                    left_bytes: 30,
                }],
            )
            .unwrap();
        }

        let stats = engine.engine_stats().await.unwrap();
        assert_eq!(stats.torrents_total, 1);
        assert_eq!(stats.torrents_downloading, 1);
        assert_eq!(stats.bytes_uploaded, 10);
        assert_eq!(stats.bytes_downloaded, 20);
        assert_eq!(stats.bytes_left, 30);
        assert_eq!(stats.jobs_active, 1);
        assert_eq!(stats.trackers_total, 1);
        assert_eq!(stats.trackers_error, 1);
        assert_eq!(stats.torrent_tasks_active, 1);
        assert_eq!(stats.fastresume_dirty_pieces, 3);
        assert_eq!(stats.completed_piece_verify_from_memory, 4);
        assert_eq!(stats.completed_piece_verify_from_disk, 5);
        assert_eq!(stats.torrents_activity_hot, 1);
        assert_eq!(stats.torrents_activity_warm, 0);
        assert_eq!(stats.torrents_activity_dormant, 0);
        assert_eq!(stats.piece_assembly_buffers, 2);
        assert_eq!(stats.piece_assembly_bytes, 4096);
        assert_eq!(stats.piece_assembly_evictions, 1);
        assert_eq!(stats.peer_request_window_reductions, 6);
        assert_eq!(stats.peer_rx_buffer_bytes, 7);
        assert_eq!(stats.peer_tx_buffer_bytes, 8);
        assert_eq!(stats.peer_command_queue_depth, 11);
        assert_eq!(stats.peer_command_queue_capacity, 12);
        assert_eq!(stats.peer_command_queue_full, 13);
        assert_eq!(stats.tracker_peer_cache_entries, 9);
        assert_eq!(stats.tracker_peer_cache_drops, 10);
        assert_eq!(stats.dht_routing_nodes, 11);
        assert_eq!(stats.dht_announced_peer_sets, 12);
        assert_eq!(stats.dht_announced_peers, 13);
        assert_eq!(stats.dht_tracked_torrents, 14);
        assert_eq!(stats.dht_outstanding_requests, 15);
        assert_eq!(stats.dht_queried_nodes, 16);
        let resources = stats.resources.expect("resource snapshot");
        let storage_frame = MemoryClass::StorageFrame as usize;
        assert_eq!(
            resources.classes[storage_frame].cap_bytes,
            StorageRuntime::global().frame_cap_bytes()
        );
        assert_eq!(
            resources.classes[MemoryClass::PieceAssembly as usize].used_bytes,
            4096
        );
        assert_eq!(
            resources.classes[MemoryClass::PeerBuffer as usize].used_bytes,
            15 + 11 * 128
        );
        assert_eq!(
            resources.classes[MemoryClass::TrackerPeers as usize].used_bytes,
            576
        );
        assert_eq!(
            resources.classes[MemoryClass::TrackerPeers as usize].denied_allocations,
            10
        );
        assert_eq!(
            resources.classes[MemoryClass::DhtTable as usize].used_bytes,
            11 * 64 + 13 * 32 + 16 * 32 + 15 * 64
        );
        assert_eq!(
            resources.classes[MemoryClass::QueuedDisk as usize].used_bytes,
            stats.storage_queued_disk_bytes
        );
        assert!(resources.total_used_bytes >= 4096);
    }

    #[tokio::test]
    async fn subsystem_health_reports_dead_dependency_seams() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let (dht_tx, dht_rx) = mpsc::channel(1);
        drop(dht_rx);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: Some(dht_tx),
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let health = engine.engine_subsystem_health().await.unwrap();
        assert!(health.dht_enabled);
        assert!(!health.dht_healthy);
        assert!(!health.storage_workers_healthy);
    }

    #[tokio::test]
    async fn reserve_memory_command_holds_and_releases_lease() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: tiny_api_snapshot_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReserveMemory {
                    class: MemoryClass::ApiSnapshot,
                    bytes: 4,
                    reply,
                })
                .await
        );
        let lease = rx.await.unwrap().unwrap().expect("lease granted");

        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReserveMemory {
                    class: MemoryClass::ApiSnapshot,
                    bytes: 1,
                    reply,
                })
                .await
        );
        assert!(rx.await.unwrap().unwrap().is_none());

        drop(lease);
        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReserveMemory {
                    class: MemoryClass::ApiSnapshot,
                    bytes: 4,
                    reply,
                })
                .await
        );
        assert!(rx.await.unwrap().unwrap().is_some());
    }

    #[tokio::test]
    async fn torrent_diagnostic_explains_paused_private_tracker_gap() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let info_hash = "f".repeat(40);
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new(info_hash.clone(), "why.bin".into(), "/tmp".into());
            entry.amount_left = 100;
            let _ = entry.transition(TorrentState::Paused);
            reg.add(entry).unwrap();
        }
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
            resources: test_resource_governor(),
            network_budget: GlobalNetworkBudget::unlimited(),
            storage_jobs: StorageJobDispatcher::for_tests(),
            tier_controller: TierController::new(TierPolicy::default()),
            tier_last_active: HashMap::new(),
            stats_cache: None,
            shutdown_reply: None,
        };
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert(
                &db,
                &TorrentRow {
                    info_hash: info_hash.clone(),
                    name: "why.bin".to_owned(),
                    total_length: 100,
                    piece_length: 10,
                    piece_count: 10,
                    is_private: true,
                    save_path: "/tmp".to_owned(),
                    category: None,
                    tags: Vec::new(),
                    state: "paused".to_owned(),
                    added_at: 1,
                    completed_at: None,
                    uploaded: 0,
                    downloaded: 0,
                    ratio: 0.0,
                    trackers: Vec::new(),
                },
            )
            .unwrap();
        }

        let diagnostic = engine.diagnose_torrent_inner(&info_hash).await.unwrap();
        assert_eq!(diagnostic.state, "paused");
        assert!(diagnostic
            .reasons
            .iter()
            .any(|reason| reason.contains("paused")));
        assert!(diagnostic
            .reasons
            .iter()
            .any(|reason| reason.contains("no persisted trackers")));
    }

    #[test]
    fn incoming_utp_listener_flag_is_boolean_only() {
        for enabled in ["1", "true", "yes", "on"] {
            assert!(parse_incoming_utp_enabled(enabled), "{enabled}");
        }
        for disabled in [
            "0", "false", "no", "off", "tcp-only", "prefer", "utp", "utp-only",
        ] {
            assert!(!parse_incoming_utp_enabled(disabled), "{disabled}");
        }
    }
}

/// Handle an incoming TCP peer connection: read the handshake's info_hash and
/// forward the stream to the matching torrent task.
async fn handle_incoming(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut hs = [0u8; HANDSHAKE_LEN];
    timeout(handshake_timeout, stream.read_exact(&mut hs))
        .await
        .context("incoming TCP peer handshake timed out")??;
    let handshake = Handshake::parse(&hs)?;
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let command = TorrentCmd::AcceptPeer {
        stream,
        peer_addr,
        handshake,
        peer_permit,
    };
    route_incoming_command(&info_hash_hex, torrent_chans, engine_tx, command).await?;
    Ok(())
}

async fn accept_utp_peer(
    endpoint: Option<&UtpEndpoint>,
) -> anyhow::Result<(UtpStream, SocketAddr)> {
    let Some(endpoint) = endpoint else {
        future::pending::<()>().await;
        unreachable!("pending future never resolves");
    };
    let stream = endpoint.accept().await?;
    let peer_addr = stream.peer_addr();
    Ok((stream, peer_addr))
}

async fn handle_incoming_utp(
    mut stream: UtpStream,
    peer_addr: SocketAddr,
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    let mut hs = [0u8; HANDSHAKE_LEN];
    timeout(handshake_timeout, stream.read_exact(&mut hs))
        .await
        .context("incoming uTP peer handshake timed out")??;
    let handshake = Handshake::parse(&hs)?;
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let command = TorrentCmd::AcceptUtpPeer {
        stream,
        peer_addr,
        handshake,
        peer_permit,
    };
    route_incoming_command(&info_hash_hex, torrent_chans, engine_tx, command).await?;
    Ok(())
}

async fn route_incoming_command(
    info_hash_hex: &str,
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    engine_tx: mpsc::Sender<EngineCmd>,
    command: TorrentCmd,
) -> anyhow::Result<()> {
    match torrent_chans.get(info_hash_hex).cloned() {
        Some(tx) => match tx.send(command).await {
            Ok(()) => Ok(()),
            Err(error) => engine_tx
                .send(EngineCmd::IncomingPeer {
                    info_hash: info_hash_hex.to_owned(),
                    command: error.0,
                })
                .await
                .map_err(|_| anyhow::anyhow!("engine stopped while routing inbound peer")),
        },
        None => engine_tx
            .send(EngineCmd::IncomingPeer {
                info_hash: info_hash_hex.to_owned(),
                command,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine stopped while routing inbound peer")),
    }
}

fn incoming_utp_enabled() -> bool {
    match std::env::var("TNG_UTP_INCOMING") {
        Ok(value) => parse_incoming_utp_enabled(&value),
        Err(_) => false,
    }
}

fn parse_incoming_utp_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}
