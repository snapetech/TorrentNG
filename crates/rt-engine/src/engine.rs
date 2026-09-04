use std::collections::{BTreeSet, HashMap, HashSet};
/// Top-level engine: manages torrent task lifecycle and incoming peer listeners.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Context;
use futures::{stream, StreamExt};
use rusqlite::Connection;
use sha1::{Digest, Sha1};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch, RwLock};
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

use rt_config::Config;
use rt_db::TorrentRow;
use rt_fastresume::{FastresumeStore, PieceState};
use rt_metainfo::{
    parse_torrent, MagnetLink, TorrentMeta, TorrentMetaV1, TorrentMetaV2, MAX_TORRENT_BYTES,
};
use rt_metrics::{
    MemoryClass, MemoryPressure, ResourceGovernor, ResourceGovernorConfig, ResourceSnapshot,
    MEMORY_CLASS_COUNT,
};
use rt_path::{StorageProfile, StorageRootId};
use rt_session::{DormantTorrent, SessionRegistry, TorrentEntry, TorrentState, TransferStats};
#[cfg(test)]
use rt_storage::StorageError;
use rt_storage::{
    runtime::StorageRuntime, DurabilityMode, MountScheduler, PlannedStorageAction,
    PreallocationMode, SchedulerConfig, StorageIoConfig, StoragePlan, StoragePlanStep, V2FileHash,
    V2FileVerifier, VerifyResult,
};
use rt_utp::UtpEndpoint;

use crate::command::{
    ActiveTorrentPeers, CmdResult, EngineCategory, EngineCmd, EngineGlobalLimits, EngineJob,
    EngineNetworkFeatures, EnginePeerSnapshot, EnginePieceState, EngineStats, EngineStorageRoot,
    EngineSubsystemHealth, EngineTorrentFile, EngineTorrentLimits, EngineTorrentMetadata,
    EngineTrackerHealth, EngineTrackerSnapshot, EngineWebseedSnapshot, PreparedTorrentTaskData,
    QueueMove, TorrentDiagnostic, TorrentPromotionAction,
};
use crate::db_worker::DbExecutor;
#[cfg(not(test))]
use crate::db_worker::DbWorker;
use crate::dht_task::{run_dht, DhtCommand, DhtTorrent};
use crate::egress_policy::OutboundEgressPolicy;
use crate::metadata_task::run_metadata_task;
use crate::network_budget::GlobalNetworkBudget;
use crate::peer_ingress::{PeerIngressBudget, PeerIngressConfig};
use crate::peer_listener;
use crate::storage_authority::ServerStorageRoots;
use crate::storage_jobs::{
    StorageJobAction, StorageJobCompletion, StorageJobDispatcher, STORAGE_JOB_STATE_COMMIT_PENDING,
};
#[path = "storage_control.rs"]
mod storage_control;
#[path = "subsystems.rs"]
mod subsystems;
use crate::tier::{DormantTorrentSnapshot, TierController, TierEvent, TierInput, TierPolicy};
use crate::torrent_task::{TorrentCmd, TorrentTask};

const EVENT_ENGINE_STARTED: &str = "engine_started";
const EVENT_TORRENT_ADDED: &str = "torrent_added";
const EVENT_MAGNET_ADDED: &str = "magnet_added";
const EVENT_METADATA_RESOLVED: &str = "metadata_resolved";
const EVENT_TORRENT_RESTORED: &str = "torrent_restored";
const EVENT_TORRENT_REMOVED: &str = "torrent_removed";
const EVENT_TORRENT_REMOVE_QUEUED: &str = "torrent_remove_queued";
const EVENT_TORRENT_REMOVE_FAILED: &str = "torrent_remove_failed";
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
const STORAGE_MOVE_COMMIT_MAX_RETRIES: u8 = 5;
const STORAGE_MOVE_COMMIT_RETRY_BASE: Duration = Duration::from_millis(250);
const ENGINE_STATS_TASK_QUERY_DEADLINE: Duration = Duration::from_millis(250);
const ENGINE_STATS_REFRESH_DEADLINE: Duration = Duration::from_secs(2);
const ENGINE_STATS_REFRESH_STALE_AFTER: Duration = Duration::from_secs(2);
pub(crate) const ENGINE_COMMAND_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const ENGINE_COMMAND_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STORAGE_PLAN_AFFECTED_TORRENTS: usize = 256;
static RECHECK_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static STORAGE_PLAN_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[cfg(not(test))]
const SESSION_EVENT_QUEUE_CAPACITY: usize = 1024;

/// Durable operator events are observability data, not engine ordering data.
/// Keep their SQLite writes behind a bounded, single-consumer queue so a slow
/// database or retention prune cannot stop the engine actor from dispatching
/// torrent/network commands. The single consumer preserves event order and
/// bounds both queued work and blocking threads.
#[cfg(not(test))]
struct SessionEventWriter {
    tx: mpsc::Sender<rt_db::SessionEventRow>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(not(test))]
impl SessionEventWriter {
    fn new(db: Arc<Mutex<Connection>>, retention: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<rt_db::SessionEventRow>(SESSION_EVENT_QUEUE_CAPACITY);
        let task = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let kind = event.kind.clone();
                let db = Arc::clone(&db);
                let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
                    let db = db
                        .lock()
                        .map_err(|_| "database mutex poisoned".to_owned())?;
                    rt_db::append_session_event(&db, &event).map_err(|error| error.to_string())?;
                    rt_db::prune_session_events(&db, retention)
                        .map_err(|error| error.to_string())?;
                    Ok(())
                })
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(
                        component = "db",
                        operation = "append_session_event",
                        kind = %kind,
                        result = "error",
                        error = %error,
                        "failed to append session event"
                    ),
                    Err(error) => warn!(
                        component = "db",
                        operation = "append_session_event",
                        kind = %kind,
                        result = "worker_failed",
                        error = %error,
                        "session event worker failed"
                    ),
                }
            }
        });
        Self {
            tx,
            task: Some(task),
        }
    }

    async fn shutdown(self, timeout_budget: Duration) {
        let Self { tx, task } = self;
        drop(tx);
        if let Some(task) = task {
            if timeout(timeout_budget, task).await.is_err() {
                warn!(
                    component = "db",
                    operation = "shutdown_session_event_writer",
                    result = "timeout",
                    "session event writer did not drain before shutdown deadline"
                );
            }
        }
    }
}

async fn await_engine_reply<T>(rx: oneshot::Receiver<CmdResult<T>>) -> CmdResult<T> {
    match timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("engine dropped reply".to_owned()),
        Err(_) => Err("engine command timed out".to_owned()),
    }
}

async fn send_torrent_command(tx: &mpsc::Sender<TorrentCmd>, command: TorrentCmd) -> CmdResult<()> {
    match timeout(ENGINE_COMMAND_SEND_TIMEOUT, tx.send(command)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("torrent task gone".to_owned()),
        Err(_) => Err("torrent task command queue timed out".to_owned()),
    }
}

fn try_broadcast_torrent_command<'a, I, F>(
    channels: I,
    mut command: F,
    operation: &str,
) -> CmdResult<()>
where
    I: IntoIterator<Item = &'a mpsc::Sender<TorrentCmd>>,
    F: FnMut() -> TorrentCmd,
{
    let mut failures = 0usize;
    let mut first_error = None;
    for tx in channels {
        if let Err(error) = tx.try_send(command()) {
            failures = failures.saturating_add(1);
            first_error.get_or_insert_with(|| error.to_string());
            debug!(
                component = "engine",
                operation,
                result = "not_delivered",
                error = %error,
                "torrent task did not accept a broadcast command"
            );
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation} was not delivered to {failures} active torrent task(s); first error: {}",
            first_error.unwrap_or_else(|| "unknown delivery failure".to_owned())
        ))
    }
}

async fn quiesce_torrent_channel(tx: &mpsc::Sender<TorrentCmd>) -> CmdResult<bool> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    send_torrent_command(tx, TorrentCmd::QuiesceForStorageMove { reply }).await?;
    timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
        .await
        .map_err(|_| "torrent task quiesce timed out".to_owned())?
        .map_err(|_| "torrent task dropped quiesce reply".to_owned())
}

const SETTING_GLOBAL_DOWNLOAD_LIMIT: &str = "transfer.download_limit";
const SETTING_GLOBAL_UPLOAD_LIMIT: &str = "transfer.upload_limit";
const SETTING_GLOBAL_SPEED_LIMITS_MODE: &str = "transfer.speed_limits_mode";
const SETTING_NETWORK_DHT: &str = "network.dht";
const SETTING_NETWORK_PEX: &str = "network.pex";
const SETTING_NETWORK_USER_AGENT: &str = "network.user_agent";
const SETTING_QUEUE_PREFIX: &str = "torrent.queue.";
const SETTING_GLOBAL_TAGS: &str = "labels.tags";
const ENGINE_STATS_CACHE_TTL: Duration = Duration::from_millis(500);
const TIER_IDLE_RECONCILE_MAX_PER_TICK: usize = 256;
const TIER_IDLE_RECONCILE_CONCURRENCY: usize = 64;
const TIER_IDLE_RETRY_DELAY: Duration = Duration::from_secs(1);

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
    let deadline = tokio::time::Instant::now() + timeout_budget;
    let Some(send_budget) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
        return;
    };
    match timeout(send_budget, tx.send(DhtCommand::Shutdown { reply })).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return,
    }
    let Some(wait_budget) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
        return;
    };
    if timeout(wait_budget, rx).await.is_err() {
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
    alive: Arc<AtomicBool>,
    task: Arc<EngineTaskControl>,
}

struct EngineTaskControl {
    abort: Option<tokio::task::AbortHandle>,
    shutdown_timeout: Duration,
    peer_listener_healthy: Option<Arc<AtomicBool>>,
}

impl EngineHandle {
    /// Return whether the engine actor task is still running and owns its
    /// command receiver. The separate liveness flag becomes false when the
    /// actor exits, panics, or is cancelled; a sender-only check would report
    /// healthy until the channel happened to be dropped.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire) && !self.tx.is_closed()
    }

    /// Return whether the engine's TCP peer listener is accepting work. Test
    /// handles without a listener treat this seam as healthy; a running
    /// daemon supplies the shared health flag owned by its actor.
    pub fn peer_listener_healthy(&self) -> bool {
        self.task
            .peer_listener_healthy
            .as_ref()
            .map(|healthy| healthy.load(Ordering::Acquire))
            .unwrap_or(true)
    }

    /// Enqueue a command without allowing a saturated actor mailbox to pin
    /// an API task forever. A bounded send is part of the fault-isolation
    /// contract: a dead or wedged actor must fail the caller, not turn every
    /// subsequent request into an unbounded waiter.
    async fn send_command(&self, command: EngineCmd) -> CmdResult<()> {
        match timeout(ENGINE_COMMAND_SEND_TIMEOUT, self.tx.send(command)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("engine shut down".to_owned()),
            Err(_) => Err("engine command queue timed out".to_owned()),
        }
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
        self.send_command(EngineCmd::AddTorrent {
            meta: Box::new(meta),
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    /// Add a torrent from raw metainfo. Parsing and persistence are detached
    /// from the engine actor, which keeps a large bencoded file from blocking
    /// lifecycle, health, or peer-routing commands.
    pub async fn add_torrent_raw_with_labels(
        &self,
        raw: Vec<u8>,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddTorrentRaw {
            raw,
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
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
        self.send_command(EngineCmd::AddMagnet {
            magnet,
            save_path,
            paused,
            category,
            tags,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_torrent(
        &self,
        info_hash: String,
        delete_files: bool,
    ) -> CmdResult<Option<String>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveTorrent {
            info_hash,
            delete_files,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn pause_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::PauseTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn resume_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ResumeTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn recheck_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RecheckTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn pause_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::PauseJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn resume_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ResumeJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn cancel_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CancelJob { job_id, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn reannounce_torrent(&self, info_hash: String) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ReannounceTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_metadata(&self, info_hash: String) -> CmdResult<EngineTorrentMetadata> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentMetadata { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_blob(&self, info_hash: String) -> CmdResult<Vec<u8>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentBlob { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_trackers(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineTrackerSnapshot>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentTrackers { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn tracker_health(&self) -> CmdResult<Vec<EngineTrackerHealth>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTrackerHealth { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_hashes_by_tracker(&self, tracker: String) -> CmdResult<Vec<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListTorrentHashesByTracker { tracker, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn get_setting(&self, key: String) -> CmdResult<Option<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetSetting { key, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn set_setting(&self, key: String, value: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::SetSetting { key, value, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn execute_storage_plan(
        &self,
        operation: String,
        affected_torrents: Vec<String>,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
    ) -> CmdResult<String> {
        let affected_torrents = affected_torrents
            .into_iter()
            .map(canonical_info_hash)
            .collect();
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ExecuteStoragePlan {
            operation,
            affected_torrents,
            plan,
            completed_steps,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_jobs(&self) -> CmdResult<Vec<EngineJob>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListJobs { reply }).await?;
        await_engine_reply(rx).await
    }

    pub async fn list_storage_roots(&self) -> CmdResult<Vec<EngineStorageRoot>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListStorageRoots { reply })
            .await?;
        await_engine_reply(rx).await
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
        self.send_command(EngineCmd::GetStats { reply }).await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "engine stats command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetHealth { reply }).await?;
        timeout(Duration::from_millis(500), rx)
            .await
            .map_err(|_| "engine health command timed out".to_owned())?
            .map_err(|_| "engine dropped reply".to_owned())?
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
        self.send_command(EngineCmd::ListSessionEvents {
            info_hash,
            kind,
            levels,
            last_known_id,
            limit,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn reserve_memory(
        &self,
        class: MemoryClass,
        bytes: u64,
    ) -> CmdResult<Option<rt_metrics::MemoryLease>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ReserveMemory {
            class,
            bytes,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn diagnose_torrent(&self, info_hash: String) -> CmdResult<TorrentDiagnostic> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::DiagnoseTorrent { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_labels(
        &self,
        info_hash: String,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentLabels {
            info_hash,
            category,
            add_tags,
            remove_tags,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_categories(&self) -> CmdResult<Vec<EngineCategory>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListCategories { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn create_category(&self, name: String, save_path: Option<String>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CreateCategory {
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_category(
        &self,
        old_name: String,
        new_name: String,
        save_path: Option<String>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameCategory {
            old_name,
            new_name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_categories(&self, names: Vec<String>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveCategories { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn list_tags(&self) -> CmdResult<Vec<String>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::ListTags { reply }).await?;
        await_engine_reply(rx).await
    }

    pub async fn create_tags(&self, names: Vec<String>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::CreateTags { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn remove_tags(&self, names: Vec<String>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RemoveTags { names, reply })
            .await?;
        await_engine_reply(rx).await
    }

    /// Ban peer endpoints in the engine-owned policy set. The set is shared
    /// with every torrent task, so a ban applies immediately to active tasks
    /// and to incoming connections that would otherwise trigger promotion.
    pub async fn ban_peers(&self, peers: Vec<SocketAddr>) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::BanPeers { peers, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn banned_peers(&self) -> CmdResult<Vec<SocketAddr>> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetBannedPeers { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_fields(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentFields {
            info_hash,
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_fields_with_job(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentFieldsWithJob {
            info_hash,
            name,
            save_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_trackers(
        &self,
        info_hash: String,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentTrackers {
            info_hash,
            trackers,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_limits(&self, info_hash: String) -> CmdResult<EngineTorrentLimits> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentLimits { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_torrent_limits(
        &self,
        info_hash: String,
        limits: EngineTorrentLimits,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateTorrentLimits {
            info_hash,
            limits,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_file_priorities(
        &self,
        info_hash: String,
        file_ids: Vec<u32>,
        priority: i64,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateFilePriorities {
            info_hash,
            file_ids,
            priority,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_file_path(
        &self,
        info_hash: String,
        file_id: u32,
        new_path: String,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameFilePath {
            info_hash,
            file_id,
            new_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn rename_folder_path(
        &self,
        info_hash: String,
        old_path: String,
        new_path: String,
    ) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::RenameFolderPath {
            info_hash,
            old_path,
            new_path,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn add_peers(&self, info_hash: String, peers: Vec<SocketAddr>) -> CmdResult<()> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::AddPeers {
            info_hash,
            peers,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_peers(&self, info_hash: String) -> CmdResult<Vec<EnginePeerSnapshot>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentPeers { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn active_torrent_peers(&self) -> CmdResult<ActiveTorrentPeers> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetActiveTorrentPeers { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn torrent_webseeds(
        &self,
        info_hash: String,
    ) -> CmdResult<Vec<EngineWebseedSnapshot>> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetTorrentWebseeds { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn global_limits(&self) -> CmdResult<EngineGlobalLimits> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetGlobalLimits { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_global_limits(&self, limits: EngineGlobalLimits) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateGlobalLimits { limits, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn network_features(&self) -> CmdResult<EngineNetworkFeatures> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetNetworkFeatures { reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_network_features(&self, features: EngineNetworkFeatures) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateNetworkFeatures { features, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn set_user_agent(&self, user_agent: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::SetUserAgent { user_agent, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn queue_priority(&self, info_hash: String) -> CmdResult<i32> {
        let info_hash = canonical_info_hash(info_hash);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::GetQueuePriority { info_hash, reply })
            .await?;
        await_engine_reply(rx).await
    }

    pub async fn update_queue_order(
        &self,
        info_hashes: Vec<String>,
        queue_move: QueueMove,
    ) -> CmdResult<()> {
        let info_hashes = info_hashes.into_iter().map(canonical_info_hash).collect();
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_command(EngineCmd::UpdateQueueOrder {
            info_hashes,
            queue_move,
            reply,
        })
        .await?;
        await_engine_reply(rx).await
    }

    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + self.task.shutdown_timeout;
        let send_budget = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        match timeout(send_budget, self.tx.send(EngineCmd::Shutdown { reply })).await {
            Ok(Ok(())) => {
                let wait_budget = deadline
                    .checked_duration_since(tokio::time::Instant::now())
                    .unwrap_or_default();
                if timeout(wait_budget, rx).await.is_err() {
                    warn!(
                        component = "engine",
                        operation = "shutdown",
                        result = "timeout",
                        "engine actor did not stop before the shutdown deadline; aborting it"
                    );
                    if let Some(abort) = &self.task.abort {
                        abort.abort();
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(_) => {
                warn!(
                    component = "engine",
                    operation = "shutdown",
                    result = "timeout",
                    "engine command channel did not accept shutdown before the deadline; aborting it"
                );
                if let Some(abort) = &self.task.abort {
                    abort.abort();
                }
            }
        }
    }
}

/// The running engine.
pub struct Engine {
    config: Arc<Config>,
    registry: Arc<RwLock<SessionRegistry>>,
    #[cfg(test)]
    db: Arc<Mutex<Connection>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    cmd_tx: mpsc::Sender<EngineCmd>,
    runtime: subsystems::EngineRuntimeState,
    services: subsystems::EngineSubsystems,
    shutdown_reply: Option<oneshot::Sender<()>>,
    #[cfg(not(test))]
    db_worker: DbWorker,
    #[cfg(not(test))]
    session_event_writer: Option<SessionEventWriter>,
}

struct EngineLivenessGuard {
    alive: Arc<AtomicBool>,
    peer_listener_stop: watch::Sender<bool>,
}

#[derive(Debug)]
struct StorageDeleteCompletion {
    job_id: String,
    info_hash: String,
    succeeded: bool,
    terminal_state: String,
    error: Option<String>,
    completed_steps: Vec<usize>,
    quiesced: Vec<(String, bool)>,
}

#[derive(Debug)]
struct PureV2RecheckCompletion {
    info_hash: String,
    job_id: Option<String>,
    total_length: u64,
    total_files: i64,
    done: i64,
    invalid_files: Vec<i64>,
    error: Option<String>,
}

/// Inputs captured by the actor before a stats refresh is detached. The
/// resulting collector never borrows actor state; it reads only cloned
/// channels and shared read models, then posts one immutable result back.
struct EngineStatsRefreshInput {
    registry: Arc<RwLock<SessionRegistry>>,
    db: DbExecutor,
    task_channels: Vec<(String, mpsc::Sender<TorrentCmd>)>,
    dht_tx: Option<mpsc::Sender<DhtCommand>>,
    storage_jobs_inflight: u64,
    storage_jobs_queue_depth: u64,
    storage_jobs_capacity: u64,
    storage_workers: u64,
    storage_workers_healthy: u64,
    resources: ResourceGovernor,
    tier_counts: [usize; 3],
    dormant_runtime_heap_bytes: u64,
    pressure_constrained_pct: u8,
    pressure_critical_pct: u8,
}

enum TorrentPromotionBegin {
    Ready(Box<TorrentPromotionAction>),
    Pending,
}

impl Drop for EngineLivenessGuard {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = self.peer_listener_stop.send(true);
    }
}

impl Engine {
    /// Execute database work through the supervised engine-owned boundary.
    /// Production callers never receive the actor's SQLite mutex; tests use
    /// their existing direct fixture so small in-process state-machine tests
    /// do not need to start a file-backed supervisor.
    #[cfg(not(test))]
    async fn run_db<T, F>(&self, _operation: &'static str, job: F) -> CmdResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        self.db_worker.run(_operation, job).await
    }

    #[cfg(test)]
    async fn run_db<T, F>(&self, operation: &'static str, job: F) -> CmdResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        DbExecutor::direct(Arc::clone(&self.db))
            .run(operation, job)
            .await
    }

    fn db_executor(&self) -> DbExecutor {
        #[cfg(not(test))]
        {
            DbExecutor::worker(self.db_worker.clone())
        }
        #[cfg(test)]
        {
            DbExecutor::direct(Arc::clone(&self.db))
        }
    }

    /// Spawn the engine, returning an EngineHandle for the API layer.
    pub async fn start(
        config: Arc<Config>,
        registry: Arc<RwLock<SessionRegistry>>,
    ) -> anyhow::Result<EngineHandle> {
        let (tx, cmd_rx) = mpsc::channel(64);
        let alive = Arc::new(AtomicBool::new(true));
        // The actor performs three bounded shutdown phases (torrent tasks,
        // storage workers, and DHT). Give the public handle one aggregate
        // budget so a wedged actor cannot hold daemon shutdown forever.
        let shutdown_timeout =
            Duration::from_secs(config.daemon.shutdown_timeout_secs.max(1).saturating_mul(4));
        rt_storage::create_dir_all_no_follow(&config.daemon.session_dir)
            .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;
        rt_storage::create_dir_all_no_follow(&torrent_blob_dir(&config))
            .with_context(|| "creating torrent metadata directory")?;
        rt_storage::create_dir_all_no_follow(&fastresume_dir(&config))
            .with_context(|| "creating fastresume directory")?;
        let conn = Connection::open(config.db_path())
            .with_context(|| format!("opening database {:?}", config.db_path()))?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("configuring database busy timeout")?;
        rt_db::migrate(&conn).context("migrating database")?;
        register_configured_storage(&conn, &config).context("registering configured storage")?;
        let db = Arc::new(Mutex::new(conn));
        #[cfg(not(test))]
        let db_worker = DbWorker::new(config.db_path());
        #[cfg(not(test))]
        let session_event_writer =
            SessionEventWriter::new(Arc::clone(&db), config.logging.event_retention);
        let storage_jobs = StorageJobDispatcher::new(&config.db_path())
            .context("opening storage worker database")?;

        let network_budget = GlobalNetworkBudget::new(
            config.network.max_peers,
            (config.network.download_rate_limit > 0).then_some(config.network.download_rate_limit),
            (config.network.upload_rate_limit > 0).then_some(config.network.upload_rate_limit),
        );
        let mut engine = Engine {
            config: config.clone(),
            registry,
            #[cfg(test)]
            db,
            cmd_rx,
            cmd_tx: tx.clone(),
            runtime: subsystems::EngineRuntimeState::new(TierController::new(
                tier_policy_from_config(&config),
            )),
            services: subsystems::EngineSubsystems::new(
                None,
                resource_config_from_config(&config),
                network_budget,
                storage_jobs,
            ),
            shutdown_reply: None,
            #[cfg(not(test))]
            db_worker,
            #[cfg(not(test))]
            session_event_writer: Some(session_event_writer),
        };
        let dht_default = config.dht.enabled;
        let dht_enabled = engine
            .run_db("load_persisted_dht_setting", move |db| {
                setting_bool_with_default_checked(db, SETTING_NETWORK_DHT, dht_default)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)
            .context("loading persisted DHT setting")?;
        if dht_enabled {
            engine.services.dht_tx = Some(spawn_dht_task(&config));
        }
        let persisted_peer_bans = engine
            .run_db("load_persisted_peer_bans", |db| {
                rt_db::list_peer_bans(db).map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)
            .context("loading durable peer bans")?;
        let persisted_user_agent = engine
            .run_db("load_persisted_user_agent", |db| {
                match rt_db::get_setting(db, SETTING_NETWORK_USER_AGENT) {
                    Ok(value) => Ok(Some(value)),
                    Err(rt_db::DbError::NotFound(_)) => Ok(None),
                    Err(error) => Err(error.to_string()),
                }
            })
            .await
            .map_err(anyhow::Error::msg)
            .context("loading persisted network user agent")?;
        if let Some(user_agent) = persisted_user_agent {
            if let Err(error) = crate::peer_id::set_user_agent(user_agent) {
                warn!(
                    component = "peer_id",
                    operation = "restore_user_agent",
                    result = "ignored",
                    error = %error,
                    "ignoring invalid persisted user agent"
                );
            }
        }
        let parsed_peer_bans = persisted_peer_bans
            .into_iter()
            .filter_map(|peer| match peer.parse::<SocketAddr>() {
                Ok(peer) => Some(peer),
                Err(error) => {
                    warn!(
                        component = "engine",
                        operation = "restore_peer_ban",
                        peer = %peer,
                        result = "ignored",
                        error = %error,
                        "ignoring malformed durable peer ban"
                    );
                    None
                }
            })
            .collect::<Vec<_>>();
        engine.registry.write().await.ban_peers(parsed_peer_bans);
        engine
            .apply_shared_global_limits_from_db()
            .await
            .map_err(|error| anyhow::anyhow!(error))
            .context("loading persisted global transfer limits")?;
        engine.append_session_event(
            None,
            EVENT_ENGINE_STARTED,
            Some("native engine started"),
            serde_json::json!({
                "listen_port": config.network.listen_port,
                "dht_enabled": dht_enabled,
            }),
        );
        engine
            .recover_interrupted_jobs_async()
            .await
            .map_err(anyhow::Error::msg)
            .context("recovering interrupted jobs")?;
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
        let peer_listener_healthy = Arc::new(AtomicBool::new(true));
        let (peer_listener_stop, peer_listener_stop_rx) = watch::channel(false);
        let listener_health = Arc::clone(&peer_listener_healthy);
        let listener_network_budget = engine.services.network_budget.clone();
        let listener_engine_tx = engine.cmd_tx.clone();
        tokio::spawn(async move {
            peer_listener::run(
                listener,
                utp_endpoint,
                peer_ingress,
                listener_network_budget,
                listener_engine_tx,
                peer_listener_stop_rx,
                listener_health,
            )
            .await;
        });
        let task_alive = Arc::clone(&alive);
        let task_peer_listener_stop = peer_listener_stop.clone();
        let task = tokio::spawn(async move {
            let _liveness = EngineLivenessGuard {
                alive: task_alive,
                peer_listener_stop: task_peer_listener_stop,
            };
            engine.run(peer_listener_stop).await;
        });
        Ok(EngineHandle {
            tx,
            alive: Arc::clone(&alive),
            task: Arc::new(EngineTaskControl {
                abort: Some(task.abort_handle()),
                shutdown_timeout,
                peer_listener_healthy: Some(peer_listener_healthy),
            }),
        })
    }

    async fn run(mut self, peer_listener_stop: watch::Sender<bool>) {
        let mut tier_tick = tokio::time::interval(Duration::from_secs(5));
        let mut task_health_tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                command = self.cmd_rx.recv() => {
                    let Some(cmd) = command else {
                        // A closed command channel means the actor no longer
                        // has an owner. Leaving the listener/timers alive
                        // would make the liveness flag lie and spin a zombie
                        // engine forever.
                        warn!(
                            component = "engine",
                            operation = "run",
                            result = "command_channel_closed",
                            "engine command channel closed; shutting down"
                        );
                        break;
                    };
                    self.reap_finished_torrent_tasks().await;
                    if !self.handle_cmd(cmd).await {
                        break;
                    }
                }
                _ = tier_tick.tick() => {
                    if self.config.runtime.torrent_tiers_enabled {
                        self.promote_due_tracker_torrents(Instant::now()).await;
                        self.reconcile_activity_tiers().await;
                    }
                }
                _ = task_health_tick.tick() => {
                    self.reap_finished_torrent_tasks().await;
                }
            }
        }
        let _ = peer_listener_stop.send(true);
        self.shutdown_torrent_tasks().await;
        self.services
            .storage_jobs
            .shutdown(Duration::from_secs(
                self.config.daemon.shutdown_timeout_secs.max(1),
            ))
            .await;
        if let Some(tx) = self.services.dht_tx.take() {
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
        #[cfg(not(test))]
        if let Some(writer) = self.session_event_writer.take() {
            writer
                .shutdown(Duration::from_secs(
                    self.config.daemon.shutdown_timeout_secs.max(1),
                ))
                .await;
        }
        #[cfg(not(test))]
        self.db_worker
            .shutdown(Duration::from_secs(
                self.config.daemon.shutdown_timeout_secs.max(1),
            ))
            .await;
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
                self.fail_all_pending_torrent_promotions().await;
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
                self.begin_torrent_add(*meta, save_path, paused, category, tags, reply)
                    .await;
            }
            EngineCmd::AddTorrentRaw {
                raw,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => {
                self.start_torrent_add_from_raw(raw, save_path, paused, category, tags, reply);
            }
            EngineCmd::PreparedTorrentMeta {
                prepared,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => match prepared {
                Ok(meta) => {
                    self.begin_torrent_add(*meta, save_path, paused, category, tags, reply)
                        .await;
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            },
            EngineCmd::PreparedTorrentAdd {
                meta,
                blob,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => {
                let info_hash = meta_info_hash_hex(&meta);
                self.runtime.pending_torrent_adds.remove(&info_hash);
                let result = match blob {
                    Ok(()) => {
                        self.add_torrent_after_blob(*meta, save_path, paused, category, tags)
                            .await
                    }
                    Err(error) => Err(error),
                };
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
                let cmd_tx = self.cmd_tx.clone();
                tokio::spawn(async move {
                    let parsed = tokio::task::spawn_blocking(move || {
                        parse_torrent(&raw)
                            .map(|meta| (raw, meta))
                            .map_err(|error| error.to_string())
                    })
                    .await;
                    let (raw, meta) = match parsed {
                        Ok(Ok((raw, meta))) => (raw, Ok(meta)),
                        Ok(Err(error)) => (
                            Vec::new(),
                            Err(format!("magnet metadata parser failed: {error}")),
                        ),
                        Err(error) => (
                            Vec::new(),
                            Err(format!("magnet metadata parser worker failed: {error}")),
                        ),
                    };
                    let _ = timeout(
                        ENGINE_COMMAND_SEND_TIMEOUT,
                        cmd_tx.send(EngineCmd::PreparedMagnetMetadata {
                            info_hash,
                            raw,
                            meta,
                        }),
                    )
                    .await;
                });
            }

            EngineCmd::PreparedMagnetMetadata {
                info_hash,
                raw,
                meta,
            } => {
                if self.ensure_torrent_storage_idle(&info_hash).await.is_ok()
                    && self.ensure_torrent_exists(&info_hash).await.is_ok()
                {
                    self.start_magnet_blob_persistence(info_hash, raw, meta);
                }
            }
            EngineCmd::PreparedMagnetBlob {
                info_hash,
                meta,
                blob,
            } => {
                let result = match blob {
                    Ok(()) => match meta {
                        Ok(meta) => self.complete_magnet_persisted(&info_hash, meta).await,
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    warn!(
                        component = "engine",
                        operation = "complete_magnet",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
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

            EngineCmd::BanPeers { peers, reply } => {
                let accepted = self.registry.read().await.bannable_peers(peers);
                let result = if accepted.is_empty() {
                    Ok(())
                } else {
                    let accepted_for_db = accepted.clone();
                    let mut result = self
                        .run_db("ban_peers", move |db| {
                            let tx = db.transaction().map_err(|error| error.to_string())?;
                            rt_db::insert_peer_bans_in_tx(&tx, &accepted_for_db, unix_now_i64())
                                .map_err(|error| error.to_string())?;
                            tx.commit().map_err(|error| error.to_string())
                        })
                        .await;
                    if result.is_ok() {
                        self.registry
                            .write()
                            .await
                            .ban_peers(accepted.iter().copied());
                        // A ban is not effective if an already-connected peer
                        // remains in a torrent task. Admission checks cover
                        // future connections; this fan-out evicts the current
                        // session and releases its scheduler/peer permit.
                        let mut delivery_failures = 0usize;
                        let mut first_delivery_error = None;
                        for peer in accepted.iter().copied() {
                            for tx in self.runtime.torrent_chans.values() {
                                if let Err(error) = tx.try_send(TorrentCmd::BanPeer(peer)) {
                                    delivery_failures = delivery_failures.saturating_add(1);
                                    first_delivery_error.get_or_insert_with(|| error.to_string());
                                    debug!(
                                        component = "engine",
                                        operation = "evict_banned_peer",
                                        peer = %peer,
                                        result = "not_delivered",
                                        error = %error,
                                        "could not deliver active-peer ban to torrent task"
                                    );
                                }
                            }
                        }
                        if delivery_failures > 0 {
                            result = Err(format!(
                                "peer ban persisted but was not delivered to {delivery_failures} active torrent task command(s); first error: {}",
                                first_delivery_error
                                    .unwrap_or_else(|| "unknown delivery failure".to_owned())
                            ));
                        }
                    }
                    result
                };
                let _ = reply.send(result);
            }

            EngineCmd::GetBannedPeers { reply } => {
                let result = self.registry.read().await.banned_peers();
                let _ = reply.send(Ok(result));
            }

            EngineCmd::RemoveTorrent {
                info_hash,
                delete_files,
                reply,
            } => {
                self.cancel_pending_torrent_promotion(
                    &info_hash,
                    "torrent promotion cancelled because the torrent was removed",
                )
                .await;
                let result = self.remove_torrent_inner(&info_hash, delete_files).await;
                let _ = reply.send(result);
            }

            EngineCmd::PauseTorrent { info_hash, reply } => {
                if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
                    let _ = reply.send(Err(error));
                    return true;
                }
                self.cancel_pending_torrent_promotion(
                    &info_hash,
                    "torrent promotion cancelled because the torrent was paused",
                )
                .await;
                self.unregister_dht_torrent(&info_hash).await;
                let active_recheck_job = match self.active_torrent_job(&info_hash).await {
                    Ok(Some((job_id, kind))) if kind == JOB_KIND_RECHECK => Some(job_id),
                    Ok(_) => None,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let mut event_persisted_with_state = false;
                let placeholder = match self.metadata_placeholder_row_checked(&info_hash).await {
                    Ok(row) => row,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let taskless_v2 = !self.runtime.torrent_chans.contains_key(&info_hash)
                    && placeholder.is_none()
                    && self.is_pure_v2_torrent(&info_hash);
                let result = if taskless_v2 {
                    if let Some(job_id) = active_recheck_job {
                        self.control_recheck_job(&job_id, JOB_STATE_PAUSED).await
                    } else {
                        let event = self.session_event_row(
                            Some(&info_hash),
                            EVENT_TORRENT_PAUSED,
                            Some("torrent paused"),
                            serde_json::json!({}),
                        );
                        let result = self
                            .set_registry_state_with_event(
                                &info_hash,
                                TorrentState::Paused,
                                None,
                                Some(event),
                            )
                            .await;
                        event_persisted_with_state = result.is_ok();
                        result
                    }
                } else {
                    // A missing channel is a legitimate dormant-torrent
                    // state. A present channel that rejects the command is a
                    // dead or wedged torrent actor, however; changing the
                    // durable projection anyway would report a successful
                    // pause that never reached the runtime. Reaping will
                    // mark that actor as failed and a later resume/recheck can
                    // recreate it.
                    let task_running = self.runtime.torrent_chans.contains_key(&info_hash);
                    match self.send_to_torrent(&info_hash, TorrentCmd::Pause).await {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            if placeholder.is_some() {
                                if task_running {
                                    Err("torrent task rejected pause command".to_owned())
                                } else {
                                    let event = self.session_event_row(
                                        Some(&info_hash),
                                        EVENT_TORRENT_PAUSED,
                                        Some("torrent paused"),
                                        serde_json::json!({}),
                                    );
                                    let result = self
                                        .update_metadata_placeholder_state_with_event(
                                            &info_hash,
                                            TorrentState::Paused,
                                            Some(event),
                                        )
                                        .await;
                                    event_persisted_with_state = result.is_ok();
                                    result
                                }
                            } else if task_running {
                                Err(error)
                            } else {
                                let event = self.session_event_row(
                                    Some(&info_hash),
                                    EVENT_TORRENT_PAUSED,
                                    Some("torrent paused"),
                                    serde_json::json!({}),
                                );
                                let result = self
                                    .set_registry_state_with_event(
                                        &info_hash,
                                        TorrentState::Paused,
                                        None,
                                        Some(event),
                                    )
                                    .await;
                                event_persisted_with_state = result.is_ok();
                                result
                            }
                        }
                    }
                };
                if result.is_ok() && !event_persisted_with_state {
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
                if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
                    let _ = reply.send(Err(error));
                    return true;
                }
                let placeholder = match self.metadata_placeholder_row_checked(&info_hash).await {
                    Ok(row) => row,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let v2_only_placeholder =
                    placeholder.as_ref().is_some_and(is_v2_only_placeholder_row);
                let taskless_v2 = !self.runtime.torrent_chans.contains_key(&info_hash)
                    && placeholder.is_none()
                    && self.is_pure_v2_torrent(&info_hash);
                if taskless_v2 {
                    let _ = reply.send(Err("pure v2 peer transfer is not implemented".to_owned()));
                } else if v2_only_placeholder {
                    let event = self.session_event_row(
                        Some(&info_hash),
                        EVENT_TORRENT_RESUMED,
                        Some("torrent resumed"),
                        serde_json::json!({
                            "v2_only": true,
                            "skipped": false,
                        }),
                    );
                    let result = self
                        .update_metadata_placeholder_state_with_event(
                            &info_hash,
                            TorrentState::MetadataPending,
                            Some(event),
                        )
                        .await;
                    let _ = reply.send(result);
                } else if placeholder.is_some() {
                    let result = match self.ensure_metadata_task(&info_hash).await {
                        Ok(()) => self.send_to_torrent(&info_hash, TorrentCmd::Resume).await,
                        Err(error) => Err(error),
                    };
                    if result.is_ok() {
                        self.register_dht_torrent_from_storage_or_hash(&info_hash);
                        self.append_session_event(
                            Some(&info_hash),
                            EVENT_TORRENT_RESUMED,
                            Some("torrent resumed"),
                            serde_json::json!({
                                "v2_only": false,
                                "skipped": false,
                            }),
                        );
                    }
                    let _ = reply.send(result);
                } else {
                    match self.begin_torrent_task_promotion(
                        &info_hash,
                        TorrentPromotionAction::Resume { reply },
                    ) {
                        TorrentPromotionBegin::Ready(action) => {
                            self.execute_torrent_promotion_action(&info_hash, *action, false)
                                .await;
                        }
                        TorrentPromotionBegin::Pending => {}
                    }
                }
            }

            EngineCmd::RecheckTorrent { info_hash, reply } => {
                if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
                    let _ = reply.send(Err(error));
                    return true;
                }
                let job_id = match self.create_recheck_job_async(&info_hash).await {
                    Ok(job_id) => job_id,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let job_id = Some(job_id);
                let placeholder = match self.metadata_placeholder_row_checked(&info_hash).await {
                    Ok(row) => row,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let pure_v2 = self.is_pure_v2_torrent(&info_hash);
                if pure_v2 {
                    let result = self.start_pure_v2_recheck(&info_hash, job_id.clone()).await;
                    if result.is_ok() {
                        if let Some(job_id) = &job_id {
                            self.update_job_state_best_effort(
                                job_id,
                                JOB_STATE_RUNNING,
                                None,
                                Some("recheck dispatched to torrent task"),
                            )
                            .await;
                        }
                    } else if let Some(job_id) = &job_id {
                        self.update_job_state_best_effort(
                            job_id,
                            JOB_STATE_FAILED,
                            result.as_ref().err().cloned(),
                            Some("recheck dispatch failed"),
                        )
                        .await;
                    }
                    let _ = reply.send(result);
                } else if placeholder.is_some() {
                    let result = match self.ensure_metadata_task(&info_hash).await {
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
                    };
                    self.record_recheck_dispatch(&info_hash, job_id.as_deref(), &result)
                        .await;
                    let _ = reply.send(result);
                } else {
                    match self.begin_torrent_task_promotion(
                        &info_hash,
                        TorrentPromotionAction::Recheck { job_id, reply },
                    ) {
                        TorrentPromotionBegin::Ready(action) => {
                            self.execute_torrent_promotion_action(&info_hash, *action, false)
                                .await;
                        }
                        TorrentPromotionBegin::Pending => {}
                    }
                }
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
                if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
                    let _ = reply.send(Err(error));
                    return true;
                }
                let placeholder = match self.metadata_placeholder_row_checked(&info_hash).await {
                    Ok(row) => row,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return true;
                    }
                };
                let v2_only_placeholder =
                    placeholder.as_ref().is_some_and(is_v2_only_placeholder_row);
                let taskless_v2 = !self.runtime.torrent_chans.contains_key(&info_hash)
                    && placeholder.is_none()
                    && self.is_pure_v2_torrent(&info_hash);
                if v2_only_placeholder {
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_REANNOUNCE_REQUESTED,
                        Some("tracker reannounce requested"),
                        serde_json::json!({
                            "v2_only": true,
                            "skipped": true,
                        }),
                    );
                    let _ = reply.send(Ok(()));
                } else if taskless_v2 {
                    let _ = reply.send(Err(
                        "pure v2 tracker lifecycle is not implemented".to_owned()
                    ));
                } else if placeholder.is_some() {
                    let result = match self.ensure_metadata_task(&info_hash).await {
                        Ok(()) => {
                            self.send_to_torrent(&info_hash, TorrentCmd::Reannounce)
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    if result.is_ok() {
                        self.append_session_event(
                            Some(&info_hash),
                            EVENT_REANNOUNCE_REQUESTED,
                            Some("torrent reannounce requested"),
                            serde_json::json!({
                                "v2_only": false,
                                "skipped": false,
                            }),
                        );
                    }
                    let _ = reply.send(result);
                } else {
                    match self.begin_torrent_task_promotion(
                        &info_hash,
                        TorrentPromotionAction::Reannounce { reply },
                    ) {
                        TorrentPromotionBegin::Ready(action) => {
                            self.execute_torrent_promotion_action(&info_hash, *action, false)
                                .await;
                        }
                        TorrentPromotionBegin::Pending => {}
                    }
                }
            }

            EngineCmd::GetTorrentMetadata { info_hash, reply } => {
                // Metainfo parsing and fastresume projection are blocking and
                // proportional to file/piece count. Do not await them from
                // the engine actor; the detached task owns only immutable
                // config/database handles and replies directly.
                let config = Arc::clone(&self.config);
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = match tokio::task::spawn_blocking(move || {
                        load_torrent_metadata_from_sources(&config, &db, &info_hash)
                    })
                    .await
                    {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(error) => Err(format!("metadata worker failed: {error}")),
                    };
                    let _ = reply.send(result);
                });
            }

            EngineCmd::GetTorrentBlob { info_hash, reply } => {
                let config = Arc::clone(&self.config);
                tokio::spawn(async move {
                    let result = match tokio::task::spawn_blocking(move || {
                        load_torrent_blob_from_config(&config, &info_hash)
                    })
                    .await
                    {
                        Ok(result) => result.map_err(|error| error.to_string()),
                        Err(error) => Err(format!("torrent blob worker failed: {error}")),
                    };
                    let _ = reply.send(result);
                });
            }

            EngineCmd::GetTorrentTrackers { info_hash, reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("get_torrent_trackers", move |db| {
                            let row =
                                rt_db::get(db, &info_hash).map_err(|error| error.to_string())?;
                            rt_db::list_torrent_trackers(db, &row.info_hash)
                                .map(|trackers| {
                                    trackers.into_iter().map(engine_tracker_snapshot).collect()
                                })
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("torrent tracker worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }

            EngineCmd::GetTrackerHealth { reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("get_tracker_health", move |db| {
                            rt_db::torrent_tracker_health(db)
                                .map(|rows| rows.into_iter().map(engine_tracker_health).collect())
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("tracker health worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }

            EngineCmd::ListTorrentHashesByTracker { tracker, reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_torrent_hashes_by_tracker", move |db| {
                            rt_db::list_torrent_hashes_by_tracker(db, &tracker)
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("tracker match worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }

            EngineCmd::GetSetting { key, reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("get_setting", move |db| {
                            match rt_db::get_setting(db, &key) {
                                Ok(value) => Ok(Some(value)),
                                Err(rt_db::DbError::NotFound(_)) => Ok(None),
                                Err(error) => Err(error.to_string()),
                            }
                        })
                        .await
                        .map_err(|error| format!("setting read worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }

            EngineCmd::SetSetting { key, value, reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("set_setting", move |db| {
                            let tx = db.transaction().map_err(|error| error.to_string())?;
                            rt_db::set_setting_in_tx(&tx, &key, &value, unix_now_i64())
                                .map_err(|error| error.to_string())?;
                            tx.commit().map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("setting write worker failed: {error}"));
                    let _ = reply.send(result);
                });
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
            EngineCmd::ListCategories { reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_categories", move |db| {
                            rt_db::list_category_definitions(db)
                                .map(|categories| {
                                    categories
                                        .into_iter()
                                        .map(|(name, save_path)| EngineCategory { name, save_path })
                                        .collect()
                                })
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("category read worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }
            EngineCmd::CreateCategory {
                name,
                save_path,
                reply,
            } => {
                let result = self
                    .create_category_inner(&name, save_path.as_deref())
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::RenameCategory {
                old_name,
                new_name,
                save_path,
                reply,
            } => {
                let result = self
                    .rename_category_inner(&old_name, &new_name, save_path.as_deref())
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::RemoveCategories { names, reply } => {
                let result = self.remove_categories_inner(&names).await;
                let _ = reply.send(result);
            }
            EngineCmd::ListTags { reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_tags", move |db| {
                            let mut tags = persisted_global_tags(db)?
                                .into_iter()
                                .collect::<BTreeSet<_>>();
                            for row in rt_db::list_all(db).map_err(|error| error.to_string())? {
                                tags.extend(row.tags.into_iter().filter(|tag| !tag.is_empty()));
                            }
                            Ok(tags.into_iter().collect())
                        })
                        .await
                        .map_err(|error| format!("tag read worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }
            EngineCmd::CreateTags { names, reply } => {
                let result = self.create_tags_inner(&names).await;
                let _ = reply.send(result);
            }
            EngineCmd::RemoveTags { names, reply } => {
                let result = self.remove_tags_inner(&names).await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentFields {
                info_hash,
                name,
                save_path,
                reply,
            } => {
                // Keep the legacy unit-result API while sharing the
                // asynchronous move-planning path used by the job-returning
                // API. The small forwarding task prevents the engine actor
                // from waiting on filesystem planning just to map the result.
                let (job_reply, job_result) = oneshot::channel::<CmdResult<Option<String>>>();
                tokio::spawn(async move {
                    let result = match job_result.await {
                        Ok(result) => result.map(|_| ()),
                        Err(_) => Err("engine dropped field update reply".to_owned()),
                    };
                    let _ = reply.send(result);
                });
                self.begin_update_torrent_fields(info_hash, name, save_path, job_reply)
                    .await;
            }
            EngineCmd::UpdateTorrentFieldsWithJob {
                info_hash,
                name,
                save_path,
                reply,
            } => {
                self.begin_update_torrent_fields(info_hash, name, save_path, reply)
                    .await;
            }
            EngineCmd::PreparedTorrentFields {
                info_hash,
                name,
                current_name,
                current_save_path,
                save_path,
                plan,
                reply,
            } => {
                let result = self
                    .finish_prepared_torrent_fields(
                        &info_hash,
                        name,
                        &current_name,
                        &current_save_path,
                        save_path,
                        plan,
                    )
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::PreparedTorrentTask {
                info_hash,
                prepared,
            } => {
                self.finish_prepared_torrent_task(info_hash, prepared).await;
            }
            EngineCmd::ExecuteStoragePlan {
                operation,
                affected_torrents,
                plan,
                completed_steps,
                reply,
            } => {
                return storage_control::execute_storage_plan(
                    self,
                    operation,
                    affected_torrents,
                    plan,
                    completed_steps,
                    reply,
                )
                .await
            }
            EngineCmd::StoragePlanFinished {
                job_id,
                affected_torrents,
                succeeded,
                terminal_state,
                error,
                completed_steps,
            } => {
                storage_control::finish_storage_plan(
                    self,
                    job_id,
                    affected_torrents,
                    succeeded,
                    terminal_state,
                    error,
                    completed_steps,
                )
                .await;
            }
            EngineCmd::StorageDeleteFinished {
                job_id,
                info_hash,
                succeeded,
                terminal_state,
                error,
                completed_steps,
                quiesced,
            } => {
                storage_control::finish_storage_delete(
                    self,
                    StorageDeleteCompletion {
                        job_id,
                        info_hash,
                        succeeded,
                        terminal_state,
                        error,
                        completed_steps,
                        quiesced,
                    },
                )
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
                terminal_state,
                error,
                completed_steps,
                retry_attempt,
            } => {
                storage_control::finish_storage_move(
                    self,
                    storage_control::StorageMoveCompletion {
                        job_id,
                        info_hash,
                        name,
                        old_save_path,
                        save_path,
                        quiesced,
                        succeeded,
                        terminal_state,
                        error,
                        completed_steps,
                        retry_attempt,
                    },
                )
                .await;
            }
            EngineCmd::PureV2RecheckFinished {
                info_hash,
                job_id,
                total_length,
                total_files,
                done,
                invalid_files,
                error,
            } => {
                storage_control::finish_pure_v2_recheck(
                    self,
                    PureV2RecheckCompletion {
                        info_hash,
                        job_id,
                        total_length,
                        total_files,
                        done,
                        invalid_files,
                        error,
                    },
                )
                .await;
            }
            EngineCmd::ListJobs { reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_jobs", move |db| {
                            rt_db::list_active_jobs(db)
                                .map(|jobs| jobs.into_iter().map(EngineJob::from).collect())
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("job list worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }
            EngineCmd::ListStorageRoots { reply } => {
                // Both the durable root query and any capacity projection
                // belong off the actor. A disconnected mount can make the
                // filesystem half of this response slow, while SQLite can
                // be contended by storage workers.
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_storage_roots", move |db| {
                            rt_db::list_storage_roots(db)
                                .map(|rows| rows.into_iter().map(engine_storage_root).collect())
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("storage root worker failed: {error}"));
                    let _ = reply.send(result);
                });
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
                self.begin_add_peers(info_hash, peers, reply).await;
            }
            EngineCmd::GetTorrentPeers { info_hash, reply } => {
                let result = self.torrent_peers_inner(&info_hash).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetActiveTorrentPeers { reply } => {
                let tasks = self
                    .runtime
                    .torrent_chans
                    .iter()
                    .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
                    .collect::<Vec<_>>();
                tokio::spawn(async move {
                    let results = stream::iter(tasks)
                        .map(|(info_hash, tx)| async move {
                            let (task_reply, task_result) = oneshot::channel();
                            let peers = match timeout(
                                ENGINE_COMMAND_SEND_TIMEOUT,
                                tx.send(TorrentCmd::GetPeers { reply: task_reply }),
                            )
                            .await
                            {
                                Ok(Ok(())) => {
                                    match timeout(ENGINE_STATS_TASK_QUERY_DEADLINE, task_result)
                                        .await
                                    {
                                        Ok(Ok(peers)) => Ok(peers),
                                        Ok(Err(_)) => {
                                            Err("torrent task dropped peer reply".to_owned())
                                        }
                                        Err(_) => {
                                            Err("torrent task peer query timed out".to_owned())
                                        }
                                    }
                                }
                                Ok(Err(_)) => Err("torrent task channel is closed".to_owned()),
                                Err(_) => Err("torrent task command send timed out".to_owned()),
                            };
                            peers.map(|peers| (info_hash, peers))
                        })
                        .buffer_unordered(64)
                        .collect::<Vec<_>>()
                        .await;
                    let result = results.into_iter().collect::<CmdResult<Vec<_>>>();
                    let _ = reply.send(result);
                });
            }
            EngineCmd::GetTorrentWebseeds { info_hash, reply } => {
                if let Some(tx) = self.runtime.torrent_chans.get(&info_hash).cloned() {
                    tokio::spawn(async move {
                        let (task_reply, task_result) = oneshot::channel();
                        let result = match timeout(
                            ENGINE_COMMAND_SEND_TIMEOUT,
                            tx.send(TorrentCmd::GetWebseeds { reply: task_reply }),
                        )
                        .await
                        {
                            Ok(Ok(())) => {
                                match timeout(ENGINE_COMMAND_REPLY_TIMEOUT, task_result).await {
                                    Ok(Ok(result)) => Ok(result),
                                    Ok(Err(_)) => Err("torrent task dropped reply".to_owned()),
                                    Err(_) => Err("torrent task reply timed out".to_owned()),
                                }
                            }
                            Ok(Err(_)) | Err(_) => Err("torrent task gone or busy".to_owned()),
                        };
                        let _ = reply.send(result);
                    });
                } else {
                    let config = Arc::clone(&self.config);
                    let db = self.db_executor();
                    tokio::spawn(async move {
                        let result = match tokio::task::spawn_blocking(move || {
                            load_torrent_metadata_from_sources(&config, &db, &info_hash)
                        })
                        .await
                        {
                            Ok(result) => result
                                .map(|metadata| {
                                    metadata
                                        .webseeds
                                        .into_iter()
                                        .map(|url| EngineWebseedSnapshot {
                                            url,
                                            is_downloading: false,
                                            download_rate: 0,
                                            failures: 0,
                                        })
                                        .collect()
                                })
                                .map_err(|error| error.to_string()),
                            Err(error) => Err(format!("metadata worker failed: {error}")),
                        };
                        let _ = reply.send(result);
                    });
                }
            }
            EngineCmd::GetGlobalLimits { reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("get_global_limits", move |db| {
                            Ok(EngineGlobalLimits {
                                download_limit: setting_i64_checked(
                                    db,
                                    SETTING_GLOBAL_DOWNLOAD_LIMIT,
                                )?,
                                upload_limit: setting_i64_checked(db, SETTING_GLOBAL_UPLOAD_LIMIT)?,
                                speed_limits_mode: setting_bool_checked(
                                    db,
                                    SETTING_GLOBAL_SPEED_LIMITS_MODE,
                                )?,
                            })
                        })
                        .await
                        .map_err(|error| format!("global limits worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }
            EngineCmd::UpdateGlobalLimits { limits, reply } => {
                // Global traffic is enforced by the engine-owned shared
                // bucket. Do not fan this update out to every torrent actor:
                // that adds O(active torrents) queue pressure and can report
                // a false partial failure after the shared budget already
                // changed.
                let result = self.update_global_limits_inner(limits).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetNetworkFeatures { reply } => {
                let db = self.db_executor();
                let dht_runtime_enabled = self
                    .services
                    .dht_tx
                    .as_ref()
                    .is_some_and(|tx| !tx.is_closed());
                let dht_default = self.config.dht.enabled;
                tokio::spawn(async move {
                    let result = db
                        .run("get_network_features", move |db| {
                            let dht_enabled = setting_bool_with_default_checked(
                                db,
                                SETTING_NETWORK_DHT,
                                dht_default,
                            )?;
                            let pex_enabled =
                                setting_bool_with_default_checked(db, SETTING_NETWORK_PEX, true)?;
                            Ok(EngineNetworkFeatures {
                                dht: dht_runtime_enabled && dht_enabled,
                                pex: pex_enabled,
                            })
                        })
                        .await
                        .map_err(|error| format!("network feature worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }
            EngineCmd::UpdateNetworkFeatures { features, reply } => {
                let result = self.update_network_features_inner(features).await;
                let _ = reply.send(result);
            }
            EngineCmd::SetUserAgent { user_agent, reply } => {
                let result = self.set_user_agent_inner(user_agent).await;
                let _ = reply.send(result);
            }
            EngineCmd::GetQueuePriority { info_hash, reply } => {
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("get_queue_priority", move |db| {
                            i32::try_from(setting_i64_checked(db, &queue_setting_key(&info_hash))?)
                                .map_err(|_| format!("queue position for {info_hash} exceeds i32"))
                        })
                        .await
                        .map_err(|error| format!("queue priority worker failed: {error}"));
                    let _ = reply.send(result);
                });
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
                let result = self.request_engine_stats();
                let _ = reply.send(result);
            }

            EngineCmd::StatsRefreshComplete { stats } => {
                if let Some(cache) = self.services.stats_cache.as_mut() {
                    cache.generated_at = Instant::now();
                    cache.stats = *stats;
                    cache.refresh_started_at = None;
                } else {
                    self.services.stats_cache = Some(subsystems::EngineStatsCache {
                        generated_at: Instant::now(),
                        stats: *stats,
                        refresh_started_at: None,
                    });
                }
            }

            EngineCmd::StatsRefreshFailed { error } => {
                if let Some(cache) = self.services.stats_cache.as_mut() {
                    cache.refresh_started_at = None;
                }
                warn!(
                    component = "engine",
                    operation = "collect_runtime_stats",
                    result = "unavailable",
                    error = %error,
                    "detached engine stats refresh failed; serving the last snapshot"
                );
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
                let db = self.db_executor();
                tokio::spawn(async move {
                    let result = db
                        .run("list_session_events", move |db| {
                            rt_db::list_session_events_filtered(
                                db,
                                info_hash.as_deref(),
                                kind.as_deref(),
                                &levels,
                                last_known_id,
                                limit.min(1_000),
                            )
                            .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| format!("session event worker failed: {error}"));
                    let _ = reply.send(result);
                });
            }

            EngineCmd::ReserveMemory {
                class,
                bytes,
                reply,
            } => {
                let _ = reply.send(Ok(self.services.resources.try_acquire(class, bytes)));
            }

            EngineCmd::DiagnoseTorrent { info_hash, reply } => {
                let result = self.diagnose_torrent_inner(&info_hash).await;
                let _ = reply.send(result);
            }
        }
        true
    }

    async fn begin_torrent_add(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>,
    ) {
        let info_hash = meta_info_hash_hex(&meta);
        if self.runtime.torrent_chans.contains_key(&info_hash)
            || self.runtime.pending_torrent_adds.contains(&info_hash)
            || self.registry.read().await.get(&info_hash).is_some()
        {
            let _ = reply.send(Err(format!("torrent {info_hash} already added")));
            return;
        }
        self.runtime.pending_torrent_adds.insert(info_hash);
        self.start_torrent_add_from_meta(meta, save_path, paused, category, tags, reply);
    }

    fn start_torrent_add_from_meta(
        &self,
        meta: TorrentMeta,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>,
    ) {
        let info_hash = meta_info_hash_hex(&meta);
        let raw = meta_raw(&meta).to_vec();
        let config = Arc::clone(&self.config);
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let blob_result = match tokio::task::spawn_blocking(move || {
                save_torrent_blob_from_config(&config, &info_hash, &raw)
                    .map_err(|error| error.to_string())
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(format!("torrent blob worker failed: {error}")),
            };
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::PreparedTorrentAdd {
                    meta: Box::new(meta),
                    blob: blob_result,
                    save_path,
                    paused,
                    category,
                    tags,
                    reply,
                }),
            )
            .await;
        });
    }

    fn start_torrent_add_from_raw(
        &self,
        raw: Vec<u8>,
        save_path: Option<PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
        reply: oneshot::Sender<CmdResult<String>>,
    ) {
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let prepared = match tokio::task::spawn_blocking(move || {
                let meta = parse_torrent(&raw).map_err(|error| error.to_string())?;
                Ok::<_, String>(Box::new(meta))
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(format!("torrent preparation worker failed: {error}")),
            };
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::PreparedTorrentMeta {
                    prepared,
                    save_path,
                    paused,
                    category,
                    tags,
                    reply,
                }),
            )
            .await;
        });
    }

    #[cfg(test)]
    async fn add_torrent(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash_hex = meta_info_hash_hex(&meta);
        self.save_torrent_blob(&info_hash_hex, meta_raw(&meta))
            .map_err(|error| error.to_string())?;
        self.add_torrent_after_blob(meta, save_path, paused, category, tags)
            .await
    }

    async fn add_torrent_after_blob(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash_hex = meta_info_hash_hex(&meta);

        if self.runtime.torrent_chans.contains_key(&info_hash_hex)
            || self.runtime.pending_torrent_adds.contains(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path_async(&save).await?;

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
            if let Some(mut e) = reg.get_mut(&info_hash_hex) {
                let _ = e.transition(target);
            };
        }

        let is_private = meta.is_private();
        let torrent_name = meta.name().to_owned();
        let v2_only = matches!(meta, TorrentMeta::V2(_));
        let added_event = self.session_event_row(
            Some(&info_hash_hex),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({
                "paused": paused || v2_only,
                "private": is_private,
                "name": torrent_name,
                "v2_only": v2_only,
            }),
        );
        let persisted = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(&info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing from registry"))?;
            self.persist_entry_with_event(&entry, &meta, Some(&added_event))
                .await
        };
        if let Err(error) = persisted {
            // Same rollback, and also clean up the blob the previous step
            // wrote -- left alone it would be an orphan file with nothing
            // in the registry or DB pointing at it.
            let _ = self.registry.write().await.remove(&info_hash_hex);
            if let Err(cleanup_error) =
                rt_storage::remove_file_no_follow(&torrent_blob_path(&self.config, &info_hash_hex))
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

        if let Some(v1) = meta_v1(meta) {
            let info_hash = v1.info_hash;
            let initial_state = if paused {
                TorrentState::Paused
            } else {
                TorrentState::Downloading
            };
            let _cmd_tx = self
                .spawn_torrent_task(info_hash_hex.clone(), v1, save, paused, initial_state)
                .await;
            if !paused && !is_private {
                self.register_dht_torrent(info_hash, &info_hash_hex).await;
            }
        }
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
        if self.runtime.torrent_chans.contains_key(&info_hash_hex)
            || self.runtime.pending_torrent_adds.contains(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        self.authorize_storage_path_async(&save).await?;
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
            added_at: db_i64(entry.added_at),
            completed_at: None,
            uploaded: 0,
            downloaded: 0,
            ratio: 0.0,
            trackers: magnet.trackers.clone(),
        };
        let tracker_rows = tracker_rows_from_urls(
            &entry.info_hash,
            &row.trackers,
            db_i64(entry.stats.uploaded),
            db_i64(entry.stats.downloaded),
            db_i64(entry.total_length.saturating_sub(entry.stats.downloaded)),
        );
        let added_event = self.session_event_row(
            Some(&info_hash_hex),
            EVENT_MAGNET_ADDED,
            Some("magnet added as metadata pending"),
            serde_json::json!({
                "paused": paused,
                "trackers": magnet.trackers.clone(),
                "v2_only": magnet.info_hash_v1.is_none(),
            }),
        );
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("add_magnet", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                rt_db::replace_torrent_trackers_in_tx(&tx, &entry.info_hash, &tracker_rows)
                    .map_err(|error| error.to_string())?;
                rt_db::append_session_event_in_tx(&tx, &added_event)
                    .map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            let _ = self.registry.write().await.remove(&info_hash_hex);
            return Err(error);
        }

        if let Some(info_hash) = magnet.info_hash_v1 {
            let _cmd_tx = self.spawn_metadata_task(
                info_hash,
                info_hash_hex.clone(),
                magnet.trackers.clone(),
                paused,
                if paused {
                    TorrentState::Paused
                } else {
                    TorrentState::MetadataPending
                },
            );
            if !paused {
                self.register_dht_torrent(info_hash, &info_hash_hex).await;
            }
        }
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

    fn start_magnet_blob_persistence(
        &self,
        info_hash: String,
        raw: Vec<u8>,
        meta: CmdResult<TorrentMeta>,
    ) {
        let config = Arc::clone(&self.config);
        let cmd_tx = self.cmd_tx.clone();
        let blob_info_hash = info_hash.clone();
        tokio::spawn(async move {
            let blob = if meta.is_ok() {
                match tokio::task::spawn_blocking(move || {
                    save_torrent_blob_from_config(&config, &blob_info_hash, &raw)
                        .map_err(|error| error.to_string())
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(format!("magnet blob worker failed: {error}")),
                }
            } else {
                Ok(())
            };
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::PreparedMagnetBlob {
                    info_hash,
                    meta,
                    blob,
                }),
            )
            .await;
        });
    }

    async fn complete_magnet_persisted(
        &mut self,
        info_hash_hex: &str,
        meta: TorrentMeta,
    ) -> CmdResult<()> {
        if self.runtime.pending_torrent_deletes.contains(info_hash_hex) {
            let _ =
                rt_storage::remove_file_no_follow(&torrent_blob_path(&self.config, info_hash_hex));
            return Err(format!(
                "torrent {info_hash_hex} is being removed; wait for payload cleanup"
            ));
        }
        self.ensure_torrent_storage_idle(info_hash_hex).await?;
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

        let (save, category, tags, previous_entry, previous_was_dormant) = {
            let reg = self.registry.read().await;
            let Some(entry) = reg.get(info_hash_hex) else {
                // A detached metadata/blob worker can finish after a caller
                // removed the placeholder. Do not recreate an orphaned blob
                // after the durable/session projection is gone.
                let _ = rt_storage::remove_file_no_follow(&torrent_blob_path(
                    &self.config,
                    info_hash_hex,
                ));
                return Err(format!(
                    "metadata-pending torrent {info_hash_hex} not found"
                ));
            };
            (
                PathBuf::from(&entry.save_path),
                entry.category.clone(),
                entry.tags.clone(),
                entry.clone(),
                reg.is_dormant(info_hash_hex),
            )
        };
        // Metadata completion is serialized by the engine actor, so this is
        // the authoritative user intent captured before replacing the
        // metadata task. A pause that arrived while metadata was in flight
        // must survive completion instead of being silently converted into a
        // downloading torrent.
        let start_paused = previous_entry.state == TorrentState::Paused;
        self.authorize_storage_path_async(&save).await?;

        {
            let mut reg = self.registry.write().await;
            let mut entry = reg
                .get_mut(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            entry.name = torrent_name.clone();
            entry.total_length = total_length;
            entry.amount_left = total_length;
            entry.category = category;
            entry.tags = tags;
            if v2_only || start_paused {
                let _ = entry.transition(TorrentState::Paused);
            } else {
                let _ = entry.transition(TorrentState::Downloading);
            }
        }
        let persisted = {
            let reg = self.registry.read().await;
            match reg.get(info_hash_hex) {
                Some(entry) => {
                    let resolved_event = self.session_event_row(
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
                    self.persist_entry_with_event(&entry, &meta, Some(&resolved_event))
                        .await
                }
                None => Err(anyhow::anyhow!(
                    "torrent {info_hash_hex} missing after metadata update"
                )),
            }
        };
        if let Err(error) = persisted {
            self.restore_registry_entry(info_hash_hex, previous_entry, previous_was_dormant)
                .await;
            return Err(error.to_string());
        }

        if let Some(old_tx) = self.runtime.torrent_chans.remove(info_hash_hex) {
            let _ = send_torrent_command(&old_tx, TorrentCmd::Shutdown).await;
        }
        if let Some(old_task) = self.runtime.torrent_tasks.remove(info_hash_hex) {
            tokio::spawn(async move {
                let _ = timeout(Duration::from_secs(10), old_task).await;
            });
        }
        // A v1 magnet has to use DHT while metadata is pending because its
        // private flag is not known yet. Once the authoritative metadata says
        // it is private, remove that provisional registration before the new
        // runtime task is installed. Otherwise private torrents continue
        // receiving DHT peers after completion.
        if is_private {
            self.unregister_dht_torrent(info_hash_hex).await;
        }
        if let Some(v1) = meta_v1(meta) {
            let info_hash = v1.info_hash;
            let _tx = self
                .spawn_torrent_task(
                    info_hash_hex.to_owned(),
                    v1,
                    save,
                    start_paused,
                    if start_paused {
                        TorrentState::Paused
                    } else {
                        TorrentState::Downloading
                    },
                )
                .await;
            if !start_paused && !is_private {
                self.register_dht_torrent(info_hash, info_hash_hex).await;
            }
        }
        info!(
            component = "engine",
            operation = "complete_magnet",
            torrent = %info_hash_hex,
            result = "ok",
            "magnet metadata completed"
        );
        Ok(())
    }

    #[cfg(test)]
    async fn complete_magnet(&mut self, info_hash_hex: &str, raw: Vec<u8>) -> CmdResult<()> {
        let meta = parse_torrent(&raw).map_err(|error| error.to_string())?;
        self.save_torrent_blob(info_hash_hex, &raw)
            .map_err(|error| error.to_string())?;
        self.complete_magnet_persisted(info_hash_hex, meta).await
    }

    async fn spawn_torrent_task(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMetaV1,
        save: PathBuf,
        paused: bool,
        initial_state: TorrentState,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let task = TorrentTask::new(
            meta,
            save,
            paused,
            Arc::clone(&self.registry),
            self.db_executor(),
            self.services.resources.clone(),
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
            self.peer_exchange_enabled().await,
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.services.network_budget.clone(),
            self.config.logging.event_retention,
        )
        .await;
        let handle = tokio::spawn(task.run());
        let tier_key = info_hash_hex.clone();
        self.runtime
            .torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.runtime.torrent_tasks.insert(info_hash_hex, handle);
        let now = Instant::now();
        self.runtime.tier_last_active.insert(tier_key.clone(), now);
        self.runtime.tier_controller.apply_input(
            tier_key,
            TierInput {
                state: initial_state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
        );
        cmd_tx
    }

    fn spawn_metadata_task(
        &mut self,
        info_hash: [u8; 20],
        info_hash_hex: String,
        trackers: Vec<String>,
        paused: bool,
        initial_state: TorrentState,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let handle = tokio::spawn(run_metadata_task(
            info_hash,
            info_hash_hex.clone(),
            trackers,
            cmd_rx,
            self.cmd_tx.clone(),
            self.services.resources.clone(),
            self.config.network.listen_port,
            self.config.network.max_peers,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            paused,
            OutboundEgressPolicy::from_config(&self.config.tracker),
            self.services.network_budget.clone(),
        ));
        let tier_key = info_hash_hex.clone();
        self.runtime
            .torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.runtime.torrent_tasks.insert(info_hash_hex, handle);
        let now = Instant::now();
        self.runtime.tier_last_active.insert(tier_key.clone(), now);
        self.runtime.tier_controller.apply_input(
            tier_key,
            TierInput {
                state: initial_state,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: Some(now),
                now,
            },
        );
        cmd_tx
    }

    async fn remove_torrent_inner(
        &mut self,
        info_hash: &str,
        delete_files: bool,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let save_path = {
            let registry = self.registry.read().await;
            registry
                .get(info_hash)
                .map(|entry| PathBuf::from(&entry.save_path))
                .ok_or_else(|| format!("torrent {info_hash} not found"))?
        };

        // Build the cleanup plan from the durable torrent-file projection.
        // The file paths are already persisted at add/import time, so delete
        // admission does not reread and parse an arbitrarily large metainfo
        // blob while the engine actor is handling commands. The worker still
        // revalidates every path against the persisted server roots.
        let (payload_plan, v2_only) = if delete_files {
            let info_hash_for_db = info_hash.to_owned();
            let (file_rows, v2_only) = self
                .run_db("load_torrent_payload_projection", move |db| {
                    let row =
                        rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())?;
                    Ok::<_, String>((
                        rt_db::list_torrent_files(db, &info_hash_for_db)
                            .map_err(|error| error.to_string())?,
                        row.info_hash.len() == 64,
                    ))
                })
                .await?;
            let file_entries = file_entries_from_rows(&file_rows)?;
            if file_entries.is_empty()
                && self
                    .metadata_placeholder_row_checked(info_hash)
                    .await?
                    .is_none()
            {
                return Err(format!(
                    "cannot prepare payload cleanup for torrent {info_hash}: durable file metadata is missing"
                ));
            }
            (
                self.plan_torrent_payload_delete(&save_path, &file_entries)
                    .await?,
                v2_only,
            )
        } else {
            (None, info_hash.len() == 64)
        };

        // A cleanup job may start as soon as it is queued. Quiesce first so
        // the worker cannot delete a file while the torrent task is writing
        // it. If a live task cannot acknowledge quiescence, leave the
        // torrent intact and report the admission failure.
        let quiesced = if payload_plan.is_some() {
            self.quiesce_torrent_for_storage_move(info_hash)
                .await
                .map_err(|error| {
                    format!(
                        "torrent {info_hash} could not be quiesced for payload cleanup: {error}"
                    )
                })?
        } else {
            None
        };

        let payload_delete_job_id = if let Some(plan) = payload_plan.as_ref() {
            match self
                .queue_torrent_payload_delete_job(info_hash, plan, quiesced)
                .await
            {
                Ok(job_id) => {
                    self.runtime
                        .pending_torrent_deletes
                        .insert(info_hash.to_owned());
                    Some(job_id)
                }
                Err(error) => {
                    self.resume_torrent_after_storage_move(info_hash, quiesced, None)
                        .await;
                    return Err(error);
                }
            }
        } else {
            None
        };

        if let Some(job_id) = payload_delete_job_id.as_ref() {
            // Keep the durable/session projection until the worker has
            // successfully removed the payload. A failed or cancelled delete
            // must leave an addressable torrent so an operator can retry it;
            // deleting the row before the worker completed made a permission
            // failure permanently orphan the payload.
            self.append_session_event(
                Some(info_hash),
                EVENT_TORRENT_REMOVE_QUEUED,
                Some("torrent removal queued after payload cleanup"),
                serde_json::json!({
                    "delete_files": true,
                    "payload_delete_job_id": job_id,
                    "save_path": save_path,
                }),
            );
            return Ok(payload_delete_job_id);
        }

        self.stop_torrent_task(info_hash).await;
        self.runtime.tier_controller.remove(&info_hash.to_owned());
        self.runtime.tier_last_active.remove(info_hash);

        let removal_event = self.session_event_row(
            Some(info_hash),
            EVENT_TORRENT_REMOVED,
            Some("torrent removed"),
            serde_json::json!({
                "delete_files": delete_files,
                "v2_only": v2_only,
                "payload_delete_job_id": Option::<String>::None,
                "save_path": save_path,
            }),
        );
        if let Err(error) = self
            .delete_persisted_torrent(info_hash, Some(&removal_event))
            .await
        {
            warn!(
                component = "db",
                operation = "delete_torrent",
                torrent = %info_hash,
                result = "error",
                error = %error,
                "failed to delete persisted torrent"
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_TORRENT_REMOVE_FAILED,
                Some("torrent removal could not delete its metadata"),
                serde_json::json!({
                    "delete_files": false,
                    "error": error.to_string(),
                }),
            );
            return Err(error.to_string());
        }
        {
            let mut registry = self.registry.write().await;
            registry
                .remove(info_hash)
                .map_err(|error| error.to_string())?
        };
        if !v2_only {
            self.unregister_dht_torrent(info_hash).await;
        }
        Ok(None)
    }

    async fn stop_torrent_task(&mut self, info_hash: &str) {
        if let Some(tx) = self.runtime.torrent_chans.remove(info_hash) {
            let _ = send_torrent_command(&tx, TorrentCmd::Shutdown).await;
        }
        if let Some(mut task) = self.runtime.torrent_tasks.remove(info_hash) {
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
    }

    async fn queue_torrent_payload_delete_job(
        &self,
        info_hash: &str,
        plan: &StoragePlan,
        quiesced: Option<bool>,
    ) -> Result<String, String> {
        let (completion, completion_rx) = oneshot::channel();
        let job_id = self
            .queue_storage_plan_job_with_context(
                "delete",
                vec![info_hash.to_owned()],
                plan,
                Vec::new(),
                serde_json::json!({
                    "info_hash": info_hash,
                    "save_path_cleanup": true,
                }),
                completion,
            )
            .await?;
        let cmd_tx = self.cmd_tx.clone();
        let job_id_for_task = job_id.clone();
        let info_hash_for_task = info_hash.to_owned();
        let quiesced = quiesced
            .map(|was_paused| vec![(info_hash.to_owned(), was_paused)])
            .unwrap_or_default();
        tokio::spawn(async move {
            let completion = completion_rx.await.unwrap_or_else(|_| {
                StorageJobCompletion::failed("storage worker completion channel closed", Vec::new())
            });
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::StorageDeleteFinished {
                    job_id: job_id_for_task,
                    info_hash: info_hash_for_task,
                    succeeded: completion.succeeded,
                    terminal_state: completion.state,
                    error: completion.error,
                    completed_steps: completion.completed_steps,
                    quiesced,
                }),
            )
            .await;
        });
        Ok(job_id)
    }

    /// Reap torrent actors that exited without going through an explicit
    /// removal or demotion path. A closed sender alone is not enough here:
    /// the old channel entry made a panicked task look alive, caused active
    /// gauges to lie, and made `ensure_torrent_task` refuse to recreate it.
    /// Marking the durable projection as an error contains the failure while
    /// preserving the normal resume/recheck path as the recovery action.
    async fn reap_finished_torrent_tasks(&mut self) {
        let finished = self
            .runtime
            .torrent_tasks
            .iter()
            .filter_map(|(info_hash, task)| task.is_finished().then_some(info_hash.clone()))
            .collect::<Vec<_>>();
        for info_hash in finished {
            let Some(task) = self.runtime.torrent_tasks.remove(&info_hash) else {
                continue;
            };
            let reason = match task.await {
                Ok(()) => "torrent task exited unexpectedly".to_owned(),
                Err(error) if error.is_panic() => format!("torrent task panicked: {error}"),
                Err(error) => format!("torrent task was cancelled: {error}"),
            };
            self.runtime.torrent_chans.remove(&info_hash);
            self.runtime.tier_controller.remove(&info_hash);
            self.runtime.tier_last_active.remove(&info_hash);

            let previous_entry = {
                let mut registry = self.registry.write().await;
                let previous = if let Some(mut entry) = registry.get_mut(&info_hash) {
                    let previous = entry.clone();
                    entry.set_error(reason.clone());
                    Some(previous)
                } else {
                    None
                };
                previous
            };
            let Some(_previous_entry) = previous_entry else {
                continue;
            };

            let failure_event = self.session_event_row(
                Some(&info_hash),
                "torrent_task_failed",
                Some("torrent runtime task exited and was isolated"),
                serde_json::json!({
                    "state": TorrentState::Error,
                    "error": reason,
                    "runtime_task_removed": true,
                }),
            );
            let retention = self.config.logging.event_retention;
            let info_hash_for_db = info_hash.clone();
            let persistence = self
                .run_db("persist_torrent_task_failure", move |db| {
                    let mut row =
                        rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())?;
                    row.state = TorrentState::Error.as_str().to_owned();
                    let tx = db.transaction().map_err(|error| error.to_string())?;
                    rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                    rt_db::append_session_event_in_tx(&tx, &failure_event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                    tx.commit().map_err(|error| error.to_string())
                })
                .await;
            if let Err(error) = persistence {
                warn!(
                    component = "db",
                    operation = "persist_torrent_task_failure",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to persist torrent task failure state"
                );
            }
            // The runtime task is gone regardless of whether the failure
            // projection committed. Keep the in-memory projection truthful
            // and compact, and recreate the dormant tier record so the next
            // resume/recheck request has a coherent promotion path. Restoring
            // the old healthy state here would make the API claim the torrent
            // is still running while no actor exists.
            self.runtime.tier_controller.apply_input(
                info_hash.clone(),
                TierInput {
                    state: TorrentState::Error,
                    connected_peers: 0,
                    outstanding_requests: 0,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: None,
                    now: Instant::now(),
                },
            );
            self.runtime.tier_controller.set_dormant_snapshot(
                info_hash.clone(),
                dormant_snapshot_from_fields(&info_hash, TorrentState::Error, None),
            );
            if let Err(error) = self.registry.write().await.demote(&info_hash) {
                warn!(
                    component = "tiering",
                    operation = "compact_reaped_torrent",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to compact reaped torrent registry entry"
                );
            }
            warn!(
                component = "engine",
                operation = "reap_torrent_task",
                torrent = %info_hash,
                result = "isolated",
                "torrent task failure was isolated; resume or recheck can recreate it"
            );
        }
    }

    async fn shutdown_torrent_tasks(&mut self) {
        let task_count = self.runtime.torrent_chans.len();
        for tx in self.runtime.torrent_chans.values() {
            // Do not let one wedged torrent queue serialize shutdown of every
            // other task. The join deadline below remains the final fallback.
            let _ = tx.try_send(TorrentCmd::Shutdown);
        }
        self.runtime.torrent_chans.clear();

        let timeout_secs = self.config.daemon.shutdown_timeout_secs.max(1);
        let timeout_budget = Duration::from_secs(timeout_secs);
        let deadline = Instant::now() + timeout_budget;
        let mut timed_out = false;

        for (info_hash, mut task) in std::mem::take(&mut self.runtime.torrent_tasks) {
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
        let due = self.runtime.tier_controller.pop_due_tracker_checks(now);
        if due.is_empty() {
            return;
        }
        let now_unix = unix_now_i64();
        for info_hash in due {
            if self.runtime.torrent_chans.contains_key(&info_hash)
                || self.ensure_torrent_jobs_idle(&info_hash).await.is_err()
            {
                continue;
            }
            let state = {
                let registry = self.registry.read().await;
                registry.get(&info_hash).map(|entry| entry.state)
            };
            if state != Some(TorrentState::Seeding) {
                continue;
            }
            let deadline = match self.persisted_tracker_deadline(&info_hash).await {
                Ok(Some(deadline)) => deadline,
                Ok(None) => continue,
                Err(error) => {
                    warn!(
                        component = "tiering",
                        operation = "load_tracker_deadline",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "could not load persisted tracker deadline"
                    );
                    continue;
                }
            };
            if deadline > now_unix {
                if let Some(deadline) = unix_deadline_to_instant(deadline, now_unix, now) {
                    self.runtime
                        .tier_controller
                        .schedule_tracker_check(info_hash, deadline);
                }
                continue;
            }

            match self
                .begin_torrent_task_promotion(&info_hash, TorrentPromotionAction::TrackerReannounce)
            {
                TorrentPromotionBegin::Ready(action) => {
                    self.execute_torrent_promotion_action(&info_hash, *action, false)
                        .await;
                }
                TorrentPromotionBegin::Pending => {
                    info!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "queued",
                        "queued dormant torrent promotion for persisted tracker deadline"
                    );
                }
            }
        }
    }

    /// Return the earliest persisted announce deadline for a torrent without
    /// retaining a per-torrent database object in the engine actor.
    async fn persisted_tracker_deadline(&self, info_hash: &str) -> CmdResult<Option<i64>> {
        let info_hash = info_hash.to_owned();
        self.run_db("load_tracker_deadline", move |db| {
            Ok(rt_db::list_torrent_trackers(db, &info_hash)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter_map(|tracker| tracker.next_announce_at)
                .min())
        })
        .await
    }

    async fn schedule_persisted_tracker_deadline(&mut self, info_hash: &str, now: Instant) {
        let deadline = match self.persisted_tracker_deadline(info_hash).await {
            Ok(deadline) => deadline,
            Err(error) => {
                warn!(
                    component = "tiering",
                    operation = "schedule_tracker_deadline",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "could not schedule persisted tracker deadline"
                );
                return;
            }
        };
        let Some(deadline) = deadline else { return };
        let now_unix = unix_now_i64();
        if let Some(deadline) = unix_deadline_to_instant(deadline, now_unix, now) {
            self.runtime
                .tier_controller
                .schedule_tracker_check(info_hash.to_owned(), deadline);
        }
    }

    /// Reconcile only promoted torrents whose activity deadline is due.
    /// Dormant torrents are represented by the registry/SQLite/blob and do
    /// not participate in a periodic per-torrent async loop. The old version
    /// walked every promoted actor on every five-second engine tick and
    /// awaited each runtime-stat reply serially; that made tier maintenance
    /// itself an engine-actor outage as the promoted set grew. The controller's
    /// deadline wheel is the admission list for this pass, and the pass has a
    /// hard work budget plus bounded parallel queries so a burst of deadlines
    /// cannot monopolize the actor.
    async fn reconcile_activity_tiers(&mut self) {
        if !self.config.runtime.torrent_tiers_enabled {
            return;
        }
        let now = Instant::now();
        let mut task_ids = self.runtime.tier_controller.pop_due_idle_checks(now);
        if task_ids.is_empty() {
            return;
        }
        if task_ids.len() > TIER_IDLE_RECONCILE_MAX_PER_TICK {
            let deferred = task_ids.split_off(TIER_IDLE_RECONCILE_MAX_PER_TICK);
            for info_hash in deferred {
                self.runtime
                    .tier_controller
                    .reschedule_idle_check(info_hash, now + TIER_IDLE_RETRY_DELAY);
            }
        }
        let states = {
            let registry = self.registry.read().await;
            task_ids
                .iter()
                .filter_map(|info_hash| {
                    if self.runtime.pending_torrent_deletes.contains(info_hash)
                        || !self.runtime.torrent_chans.contains_key(info_hash)
                    {
                        return None;
                    }
                    registry
                        .get(info_hash)
                        .map(|entry| (info_hash.clone(), entry.state))
                })
                .collect::<HashMap<_, _>>()
        };
        let task_queries = task_ids
            .into_iter()
            .filter_map(|info_hash| {
                let state = states.get(&info_hash).copied()?;
                let tx = self.runtime.torrent_chans.get(&info_hash).cloned()?;
                Some((info_hash, state, tx))
            })
            .collect::<Vec<_>>();
        let runtime_results = stream::iter(task_queries.into_iter().map(
            |(info_hash, state, tx)| async move {
                let runtime = {
                    let (reply, rx) = oneshot::channel();
                    if tx.try_send(TorrentCmd::GetRuntimeStats { reply }).is_err() {
                        Err("torrent task command channel is closed or full".to_owned())
                    } else {
                        match timeout(Duration::from_millis(50), rx).await {
                            Ok(Ok(runtime)) => Ok(runtime),
                            Ok(Err(_)) => {
                                Err("torrent task dropped runtime stats reply".to_owned())
                            }
                            Err(_) => Err("torrent task runtime stats query timed out".to_owned()),
                        }
                    }
                };
                (info_hash, state, runtime)
            },
        ))
        .buffer_unordered(TIER_IDLE_RECONCILE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        let mut demote = Vec::new();
        for (info_hash, state, runtime) in runtime_results {
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    // A failed actor probe is not evidence of inactivity.
                    // Treating it as zero peers can demote a live seed while
                    // its actor is merely busy or temporarily unavailable.
                    // Keep the deadline alive and retry after the actor has
                    // had a chance to recover.
                    warn!(
                        component = "tiering",
                        operation = "reconcile_activity",
                        torrent = %info_hash,
                        result = "retry",
                        error = %error,
                        "could not query torrent activity; preserving its current tier"
                    );
                    self.runtime
                        .tier_controller
                        .reschedule_idle_check(info_hash, now + TIER_IDLE_RETRY_DELAY);
                    continue;
                }
            };
            let connected = runtime.connected_peers as usize;
            let outstanding = runtime.outstanding_requests as usize;
            if connected > 0 || outstanding > 0 {
                self.runtime.tier_last_active.insert(info_hash.clone(), now);
            }
            let decision = self.runtime.tier_controller.apply_input(
                info_hash.clone(),
                TierInput {
                    state,
                    connected_peers: connected,
                    outstanding_requests: outstanding,
                    inbound_peer: false,
                    tracker_due: false,
                    last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
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
        let Some(tx) = self.runtime.torrent_chans.remove(info_hash) else {
            return;
        };
        self.unregister_dht_torrent(info_hash).await;
        let _ = send_torrent_command(&tx, TorrentCmd::Shutdown).await;
        if let Some(mut task) = self.runtime.torrent_tasks.remove(info_hash) {
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
        self.runtime.tier_controller.apply_input(
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
        self.schedule_persisted_tracker_deadline(info_hash, now)
            .await;
        let tracker_deadline = match self.persisted_tracker_deadline(info_hash).await {
            Ok(deadline) => deadline
                .and_then(|deadline| unix_deadline_to_instant(deadline, unix_now_i64(), now)),
            Err(error) => {
                warn!(
                    component = "tiering",
                    operation = "demote_tracker_deadline",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "could not retain persisted tracker deadline while demoting torrent"
                );
                None
            }
        };
        self.runtime.tier_controller.set_dormant_snapshot(
            info_hash.to_owned(),
            dormant_snapshot_from_fields(info_hash, state, tracker_deadline),
        );
        if let Err(error) = self.registry.write().await.demote(info_hash) {
            warn!(
                component = "tiering",
                operation = "demote_registry_entry",
                torrent = %info_hash,
                result = "error",
                error = %error,
                "failed to compact dormant registry entry"
            );
        }
        self.runtime.tier_last_active.remove(info_hash);
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
        if let Some(peer_addr) = torrent_command_peer_addr(&command) {
            if self.registry.read().await.is_peer_banned(peer_addr) {
                return Ok(());
            }
        }
        self.ensure_torrent_storage_idle(&info_hash).await?;
        let was_taskless = !self.runtime.torrent_chans.contains_key(&info_hash);
        if was_taskless {
            if self
                .metadata_placeholder_row_checked(&info_hash)
                .await?
                .is_some()
            {
                self.ensure_metadata_task(&info_hash).await?;
            } else {
                match self.begin_torrent_task_promotion(
                    &info_hash,
                    TorrentPromotionAction::IncomingPeer {
                        command: Box::new(command),
                    },
                ) {
                    TorrentPromotionBegin::Ready(action) => {
                        self.execute_torrent_promotion_action(&info_hash, *action, false)
                            .await;
                    }
                    TorrentPromotionBegin::Pending => {}
                }
                return Ok(());
            }
        }
        let tx = self
            .runtime
            .torrent_chans
            .get(&info_hash)
            .cloned()
            .ok_or_else(|| format!("torrent {info_hash} has no runtime task"))?;
        if was_taskless {
            send_torrent_command(&tx, TorrentCmd::Resume)
                .await
                .map_err(|error| format!("promoted torrent task stopped before resume: {error}"))?;
        }
        send_torrent_command(&tx, command).await.map_err(|error| {
            format!("promoted torrent task stopped before peer delivery: {error}")
        })?;

        let now = Instant::now();
        self.runtime.tier_last_active.insert(info_hash.clone(), now);
        let state = self
            .registry
            .read()
            .await
            .get(&info_hash)
            .map(|entry| entry.state)
            .unwrap_or(TorrentState::Downloading);
        self.runtime.tier_controller.apply_event(
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
        let mut rows = self
            .run_db("load_persisted_torrents", |db| {
                rt_db::list_all(db).map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)?;
        self.reconcile_registry_projection(&rows).await?;
        self.reconcile_persisted_projections(&mut rows).await?;
        // Resolve storage authority once for the entire restore. The old
        // path rebuilt/canonicalized the configured roots for every row,
        // turning a large restore into an avoidable O(torrents * roots)
        // filesystem/SQLite loop.
        let storage_authority = self
            .configured_storage_authority_async()
            .await
            .map_err(anyhow::Error::msg)?;
        self.repair_missing_torrent_tracker_rows_async(&rows)
            .await?;
        let tracker_deadlines = self
            .run_db("load_persisted_tracker_deadlines", |db| {
                let deadlines = rt_db::list_all_torrent_trackers(db)
                    .map_err(|error| error.to_string())?
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
                    );
                Ok(deadlines)
            })
            .await
            .map_err(anyhow::Error::msg)?;
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
            let start_task = state != TorrentState::Error
                && (!self.config.runtime.torrent_tiers_enabled
                    || should_start_task_on_restore(state));
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
                self.runtime.tier_controller.apply_input(
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
                            state,
                        );
                        // A paused metadata-pending torrent must not start
                        // DHT discovery during restore. If the torrent is
                        // resumed, the normal resume path registers it after
                        // the state transition. Metadata may still reveal a
                        // private torrent later; completion also removes the
                        // provisional registration in that case.
                        if !matches!(state, TorrentState::Paused | TorrentState::Stopped) {
                            self.register_dht_torrent(info_hash, &row.info_hash).await;
                        }
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
            if !start_task {
                let entry = dormant_entry_from_row(&row);
                {
                    let mut reg = self.registry.write().await;
                    if let Err(e) = reg.add_dormant(entry) {
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
                self.runtime.tier_controller.apply_input(
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
                        self.runtime
                            .tier_controller
                            .schedule_tracker_check(row.info_hash.clone(), deadline);
                    }
                }
                let tracker_deadline = tracker_deadlines.get(&row.info_hash).and_then(|deadline| {
                    unix_deadline_to_instant(*deadline, restore_now_unix, restore_now)
                });
                self.runtime.tier_controller.set_dormant_snapshot(
                    row.info_hash.clone(),
                    dormant_snapshot_from_row(&row, state, tracker_deadline),
                );
                dormant_restored = dormant_restored.saturating_add(1);
                continue;
            }
            let blob_path = torrent_blob_path(&self.config, &row.info_hash);
            let raw = match rt_storage::read_file_no_follow_limited(&blob_path, MAX_TORRENT_BYTES) {
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
                    self.restore_persisted_error_projection(
                        &row,
                        "torrent_blob",
                        &blob_path,
                        format!("failed to read persisted torrent metadata: {e}"),
                        false,
                    )
                    .await?;
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
                    self.restore_persisted_error_projection(
                        &row,
                        "torrent_blob",
                        &blob_path,
                        format!("persisted torrent metadata is invalid: {e}"),
                        true,
                    )
                    .await?;
                    continue;
                }
            };
            self.repair_torrent_file_projection(&row.info_hash, &meta)
                .await?;
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
                self.restore_persisted_error_projection(
                    &row,
                    "torrent_blob",
                    &blob_path,
                    format!(
                        "persisted torrent hash {info_hash_hex} does not match row {}",
                        row.info_hash
                    ),
                    true,
                )
                .await?;
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

            self.runtime.tier_controller.apply_input(
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
                    let _tx = self
                        .spawn_torrent_task(
                            row.info_hash.clone(),
                            v1,
                            PathBuf::from(&row.save_path),
                            matches!(state, TorrentState::Paused | TorrentState::Stopped),
                            state,
                        )
                        .await;
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

    #[cfg(test)]
    fn recover_interrupted_jobs(&self) -> anyhow::Result<()> {
        let now = unix_now_i64();
        let mut db = self.db.lock().expect("database mutex poisoned");
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
            let event = rt_db::JobEventRow {
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
            };
            let tx = db.transaction()?;
            rt_db::upsert_job_in_tx(&tx, &job)?;
            rt_db::append_job_event_in_tx(&tx, &event)?;
            tx.commit()?;
        }
        Ok(())
    }

    async fn recover_interrupted_jobs_async(&self) -> Result<(), String> {
        let now = unix_now_i64();
        self.run_db("recover_interrupted_jobs", move |db| {
            let jobs = rt_db::list_active_jobs(db).map_err(|error| error.to_string())?;
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
                let event = rt_db::JobEventRow {
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
                };
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
                rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
    }

    /// Reconstruct storage plans from their durable queue/checkpoint event and
    /// hand them back to the bounded worker supervisor after restart. The
    /// worker owns the filesystem transaction; the actor only restores
    /// quiesce/resume and save-path finalization callbacks.
    async fn resume_recovered_storage_jobs(&mut self) -> anyhow::Result<()> {
        let jobs = self
            .run_db("list_recoverable_storage_jobs", |db| {
                rt_db::list_active_jobs(db).map_err(|error| error.to_string())
            })
            .await
            .map_err(anyhow::Error::msg)?;
        for job in jobs.into_iter().filter(|job| {
            job.kind == JOB_KIND_STORAGE_PLAN
                && matches!(
                    job.state.as_str(),
                    JOB_STATE_QUEUED | JOB_STATE_PAUSED | STORAGE_JOB_STATE_COMMIT_PENDING
                )
        }) {
            let job_id_for_db = job.job_id.clone();
            let (events, first_event) = self
                .run_db("load_storage_job_recovery_events", move |db| {
                    Ok::<_, String>((
                        rt_db::list_job_events(db, &job_id_for_db, 64)
                            .map_err(|error| error.to_string())?,
                        rt_db::first_job_event(db, &job_id_for_db)
                            .map_err(|error| error.to_string())?,
                    ))
                })
                .await
                .map_err(anyhow::Error::msg)?;
            let Some((operation, plan, event_completed_steps, event_context)) = events
                .iter()
                .find_map(|event| decode_storage_plan_event(&event.payload))
            else {
                self.update_job_state_best_effort(
                    &job.job_id,
                    JOB_STATE_FAILED,
                    Some("storage plan payload is missing or invalid".to_owned()),
                    Some("storage plan recovery failed"),
                )
                .await;
                continue;
            };
            // Checkpoint events intentionally omit the original move context.
            // Read that context from the oldest queued event instead of
            // assuming the newest of the last 64 events contains it; a large
            // plan may have hundreds or thousands of checkpoint records.
            let context = first_event
                .as_ref()
                .and_then(|event| decode_storage_plan_context(&event.payload))
                .or_else(|| {
                    events
                        .iter()
                        .find_map(|event| decode_storage_plan_context(&event.payload))
                })
                .unwrap_or(event_context);
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
            let roots = match self.configured_storage_roots_for_execution_async().await {
                Ok(roots) => roots,
                Err(error) => {
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(error),
                        Some("storage plan recovery could not resolve roots"),
                    )
                    .await;
                    continue;
                }
            };
            let checkpoint_steps = if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                // A commit-pending move is finalized by the actor immediately
                // below, so it must prove that every filesystem step is live
                // before publishing the new registry path. Ordinary queued
                // and paused jobs are reconciled by the detached worker after
                // this startup hand-off; they must not make the actor walk
                // filesystem state while recovering a queue.
                match rt_storage::reconcile_storage_plan_under_roots(
                    &plan,
                    &roots,
                    &checkpoint_steps,
                ) {
                    Ok(steps) => steps,
                    Err(error) => {
                        self.update_job_state_best_effort(
                            &job.job_id,
                            JOB_STATE_FAILED,
                            Some(format!(
                                "storage plan filesystem reconciliation failed: {error}"
                            )),
                            Some("storage plan recovery found ambiguous filesystem state"),
                        )
                        .await;
                        continue;
                    }
                }
            } else {
                checkpoint_steps
            };

            let move_context = if operation == "move" {
                if job.affected_torrents.len() != 1 {
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(
                            "storage move job must have exactly one affected torrent context"
                                .to_owned(),
                        ),
                        Some("storage move recovery failed"),
                    )
                    .await;
                    continue;
                }
                let Some(info_hash) = job.affected_torrents.first().cloned() else {
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some("storage move job has no affected torrent context".to_owned()),
                        Some("storage move recovery failed"),
                    )
                    .await;
                    continue;
                };
                let (old_save_path, save_path, name) =
                    match Self::storage_move_context_for_plan(&plan, &context) {
                        Ok(context) => context,
                        Err(error) => {
                            self.update_job_state_best_effort(
                                &job.job_id,
                                JOB_STATE_FAILED,
                                Some(error),
                                Some("storage move recovery failed"),
                            )
                            .await;
                            continue;
                        }
                    };
                if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                    let current_save_path = {
                        let registry = self.registry.read().await;
                        let Some(entry) = registry.get(&info_hash) else {
                            self.update_job_state_best_effort(
                                &job.job_id,
                                JOB_STATE_FAILED,
                                Some(format!(
                                    "torrent {info_hash} is missing during storage move recovery"
                                )),
                                Some("storage move recovery failed"),
                            )
                            .await;
                            continue;
                        };
                        PathBuf::from(&entry.save_path)
                    };
                    if current_save_path != old_save_path && current_save_path != save_path {
                        self.update_job_state_best_effort(
                            &job.job_id,
                            JOB_STATE_FAILED,
                            Some(format!(
                                "storage move recovery registry path {} is neither the plan source {} nor destination {}",
                                current_save_path.display(),
                                old_save_path.display(),
                                save_path.display()
                            )),
                            Some("storage move recovery found an inconsistent save path"),
                        )
                        .await;
                        continue;
                    }
                } else if let Err(error) = self
                    .storage_plan_move_context(&job.affected_torrents, &plan)
                    .await
                {
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(error),
                        Some("storage move recovery found an inconsistent source path"),
                    )
                    .await;
                    continue;
                }
                Some((info_hash, name, old_save_path, save_path))
            } else {
                None
            };
            let delete_info_hash = if operation == "delete" {
                context
                    .get("info_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            };
            if operation == "delete" && delete_info_hash.is_none() {
                self.update_job_state_best_effort(
                    &job.job_id,
                    JOB_STATE_FAILED,
                    Some("delete storage plan has no torrent info hash".to_owned()),
                    Some("storage delete recovery failed"),
                )
                .await;
                continue;
            }
            // Older delete jobs did not populate affected_torrents, so use
            // the durable context as the authoritative recovery target.
            // The restored torrent must be quiesced before a delete worker is
            // allowed to touch its payload.
            let quiesce_targets = delete_info_hash
                .as_ref()
                .map(|info_hash| vec![info_hash.clone()])
                .unwrap_or_else(|| job.affected_torrents.clone());
            if let Some(info_hash) = delete_info_hash.as_ref() {
                self.runtime
                    .pending_torrent_deletes
                    .insert(info_hash.clone());
            }
            let quiesced = match self
                .quiesce_torrents_for_storage_plan(&quiesce_targets)
                .await
            {
                Ok(quiesced) => quiesced,
                Err(error) => {
                    if let Some(info_hash) = delete_info_hash.as_ref() {
                        self.runtime.pending_torrent_deletes.remove(info_hash);
                    }
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(format!(
                            "storage plan recovery could not quiesce torrent(s): {error}"
                        )),
                        Some("storage plan recovery could not quiesce torrent(s)"),
                    )
                    .await;
                    continue;
                }
            };

            if job.state == STORAGE_JOB_STATE_COMMIT_PENDING {
                if checkpoint_steps.len() != plan.steps.len() {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                    self.update_job_state_best_effort(
                        &job.job_id,
                        JOB_STATE_FAILED,
                        Some(format!(
                            "storage job claimed commit pending but filesystem reconciliation found {}/{} steps",
                            checkpoint_steps.len(),
                            plan.steps.len()
                        )),
                        Some("storage plan commit-pending recovery found incomplete filesystem state"),
                    )
                    .await;
                    continue;
                }
                if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                    let quiesced_for_move = quiesced
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, paused)| *paused);
                    if let Err(error) = self
                        .finish_storage_move(
                            &job.job_id,
                            &info_hash,
                            name,
                            old_save_path,
                            save_path,
                            quiesced_for_move,
                            true,
                            STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                            None,
                            checkpoint_steps,
                            0,
                        )
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "recover_commit_pending_move",
                            job_id = %job.job_id,
                            result = "error",
                            error = %error,
                            "storage move commit remains pending after restart"
                        );
                    }
                } else {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                    if let Err(error) = self
                        .complete_storage_plan_job_async(&job.job_id, &checkpoint_steps)
                        .await
                    {
                        warn!(
                            component = "storage_jobs",
                            operation = "recover_commit_pending_plan",
                            job_id = %job.job_id,
                            result = "error",
                            error = %error,
                            "storage plan commit remains pending after restart"
                        );
                    }
                }
                continue;
            }

            let (completion, completion_rx) = oneshot::channel();
            let submit_result = {
                #[cfg(not(test))]
                {
                    if job.state == JOB_STATE_PAUSED {
                        self.services.storage_jobs.submit_paused_managed(
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else {
                        self.services.storage_jobs.submit_managed(
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    }
                }
                #[cfg(test)]
                {
                    if job.state == JOB_STATE_PAUSED {
                        self.services.storage_jobs.submit_paused(
                            Arc::clone(&self.db),
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    } else {
                        self.services.storage_jobs.submit(
                            Arc::clone(&self.db),
                            job.job_id.clone(),
                            operation.clone(),
                            plan,
                            checkpoint_steps,
                            roots,
                            completion,
                        )
                    }
                }
            };
            if let Err(error) = submit_result {
                if let Some(info_hash) = delete_info_hash.as_ref() {
                    self.runtime.pending_torrent_deletes.remove(info_hash);
                }
                self.resume_torrents_after_storage_plan(quiesced).await;
                self.update_job_state_best_effort(
                    &job.job_id,
                    JOB_STATE_FAILED,
                    Some(error),
                    Some("storage plan recovery could not be queued"),
                )
                .await;
                continue;
            }

            let cmd_tx = self.cmd_tx.clone();
            let job_id = job.job_id.clone();
            let affected_torrents = quiesced;
            tokio::spawn(async move {
                let completion = completion_rx.await.unwrap_or_else(|_| {
                    StorageJobCompletion::failed(
                        "storage worker completion channel closed",
                        Vec::new(),
                    )
                });
                if let Some(info_hash) = delete_info_hash {
                    let _ = timeout(
                        ENGINE_COMMAND_SEND_TIMEOUT,
                        cmd_tx.send(EngineCmd::StorageDeleteFinished {
                            job_id,
                            info_hash,
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                            quiesced: affected_torrents,
                        }),
                    )
                    .await;
                } else if let Some((info_hash, name, old_save_path, save_path)) = move_context {
                    let quiesced = affected_torrents
                        .iter()
                        .find(|(hash, _)| hash == &info_hash)
                        .map(|(_, paused)| *paused);
                    let _ = timeout(
                        ENGINE_COMMAND_SEND_TIMEOUT,
                        cmd_tx.send(EngineCmd::StorageMoveFinished {
                            job_id,
                            info_hash,
                            name,
                            old_save_path,
                            save_path,
                            quiesced,
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                            retry_attempt: 0,
                        }),
                    )
                    .await;
                } else {
                    let _ = timeout(
                        ENGINE_COMMAND_SEND_TIMEOUT,
                        cmd_tx.send(EngineCmd::StoragePlanFinished {
                            job_id,
                            affected_torrents,
                            succeeded: completion.succeeded,
                            terminal_state: completion.state,
                            error: completion.error,
                            completed_steps: completion.completed_steps,
                        }),
                    )
                    .await;
                }
            });
        }
        Ok(())
    }

    async fn persist_entry_with_event(
        &self,
        entry: &TorrentEntry,
        meta: &TorrentMeta,
        event: Option<&rt_db::SessionEventRow>,
    ) -> anyhow::Result<()> {
        let row = row_from_entry(entry, meta);
        let files = meta_file_rows(&entry.info_hash, meta);
        let tracker_rows = tracker_rows_from_urls(
            &entry.info_hash,
            &row.trackers,
            row.uploaded,
            row.downloaded,
            row.total_length.saturating_sub(row.downloaded).max(0),
        );
        let info_hash = entry.info_hash.clone();
        let event = event.cloned();
        let retention = self.config.logging.event_retention;
        self.run_db("persist_torrent_projection", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
            rt_db::replace_torrent_files_in_tx(&tx, &info_hash, &files)
                .map_err(|error| error.to_string())?;
            rt_db::replace_torrent_trackers_in_tx(&tx, &info_hash, &tracker_rows)
                .map_err(|error| error.to_string())?;
            if let Some(event) = event.as_ref() {
                rt_db::append_session_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    fn save_torrent_blob(&self, info_hash: &str, raw: &[u8]) -> anyhow::Result<()> {
        save_torrent_blob_from_config(&self.config, info_hash, raw)
    }

    #[cfg(test)]
    fn load_torrent_blob(&self, info_hash: &str) -> anyhow::Result<Vec<u8>> {
        load_torrent_blob_from_config(&self.config, info_hash)
    }

    async fn delete_persisted_torrent(
        &self,
        info_hash: &str,
        event: Option<&rt_db::SessionEventRow>,
    ) -> anyhow::Result<()> {
        // Remove filesystem projections first. If a mount or fastresume file
        // is unavailable, retain the DB row so the operator can retry rather
        // than leaving an invisible metadata/payload split. DB deletion is
        // last; a DB failure is likewise recoverable by retrying cleanup.
        let config = Arc::clone(&self.config);
        let info_hash_owned = info_hash.to_owned();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            match rt_storage::remove_file_no_follow(&torrent_blob_path(&config, &info_hash_owned)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            FastresumeStore::new(fastresume_dir(&config)).delete(&info_hash_owned)?;
            Ok(())
        })
        .await
        .map_err(|error| anyhow::anyhow!("torrent projection cleanup worker failed: {error}"))??;
        let info_hash = info_hash.to_owned();
        let event = event.cloned();
        let retention = self.config.logging.event_retention;
        self.run_db("delete_torrent_projection", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            let _ = rt_db::delete_in_tx(&tx, &info_hash).map_err(|error| error.to_string())?;
            if let Some(event) = event.as_ref() {
                rt_db::append_session_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    #[cfg(test)]
    fn load_torrent_metadata(&self, info_hash: &str) -> anyhow::Result<EngineTorrentMetadata> {
        let db = DbExecutor::direct(Arc::clone(&self.db));
        load_torrent_metadata_from_sources(&self.config, &db, info_hash)
    }

    fn is_pure_v2_torrent(&self, info_hash: &str) -> bool {
        // v2-only rows use the 32-byte SHA-256 info hash representation. A
        // hybrid torrent still has its v1 SHA-1 identity and therefore stays
        // on the regular torrent-task path. The worker performs the
        // authoritative metadata-kind check before touching payload files.
        info_hash.len() == 64
    }

    async fn start_pure_v2_recheck(
        &self,
        info_hash: &str,
        job_id: Option<String>,
    ) -> CmdResult<()> {
        self.ensure_torrent_storage_idle(info_hash).await?;
        let save_root = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            PathBuf::from(&entry.save_path)
        };
        let authority = self.configured_storage_authority_async().await?;
        authority
            .authorize_path(&save_root)
            .map_err(|error| error.to_string())?;
        let event = self.session_event_row(
            Some(info_hash),
            EVENT_RECHECK_REQUESTED,
            Some("torrent recheck requested"),
            serde_json::json!({ "job_id": job_id }),
        );
        self.set_registry_state_with_event(info_hash, TorrentState::Checking, None, Some(event))
            .await?;
        if let Some(job_id) = &job_id {
            self.update_job_state_async(
                job_id,
                JOB_STATE_RUNNING,
                None,
                Some("pure v2 recheck dispatched to storage worker"),
            )
            .await
            .map_err(|error| format!("failed to mark pure v2 recheck running: {error}"))?;
        }

        let config = Arc::clone(&self.config);
        let resources = self.services.resources.clone();
        let cmd_tx = self.cmd_tx.clone();
        let info_hash = info_hash.to_owned();
        tokio::spawn(async move {
            let result =
                execute_pure_v2_recheck(config, resources, authority, save_root, info_hash.clone())
                    .await;
            let command = match result {
                Ok(result) => EngineCmd::PureV2RecheckFinished {
                    info_hash,
                    job_id,
                    total_length: result.total_length,
                    total_files: result.total_files,
                    done: result.done,
                    invalid_files: result.invalid_files,
                    error: None,
                },
                Err(error) => EngineCmd::PureV2RecheckFinished {
                    info_hash,
                    job_id,
                    total_length: 0,
                    total_files: 0,
                    done: 0,
                    invalid_files: Vec::new(),
                    error: Some(error),
                },
            };
            if timeout(ENGINE_COMMAND_SEND_TIMEOUT, cmd_tx.send(command))
                .await
                .is_err()
            {
                warn!(
                    component = "engine",
                    operation = "complete_pure_v2_recheck",
                    result = "completion_dropped",
                    "engine command queue remained full after pure-v2 recheck"
                );
            }
        });
        Ok(())
    }

    async fn finish_pure_v2_recheck(&self, completion: PureV2RecheckCompletion) -> CmdResult<()> {
        let PureV2RecheckCompletion {
            info_hash,
            job_id,
            total_length,
            total_files,
            done,
            invalid_files,
            error,
        } = completion;
        if self.runtime.pending_torrent_deletes.contains(&info_hash) {
            if let Some(job_id) = &job_id {
                self.update_job_state_async(
                    job_id,
                    JOB_STATE_CANCELLED,
                    Some("torrent removal superseded the recheck".to_owned()),
                    Some("pure v2 recheck discarded during torrent removal"),
                )
                .await?;
            }
            return Ok(());
        }
        if let Some(error) = error {
            if let Some(job_id) = &job_id {
                self.update_job_state_async(
                    job_id,
                    JOB_STATE_FAILED,
                    Some(error.clone()),
                    Some("pure v2 recheck failed"),
                )
                .await?;
            }
            let event = self.session_event_row(
                Some(&info_hash),
                "check_failed",
                Some("pure v2 file-root recheck failed"),
                serde_json::json!({ "error": error }),
            );
            self.set_registry_state_with_event(&info_hash, TorrentState::Error, None, Some(event))
                .await?;
            return Ok(());
        }

        // A pause/cancel can arrive while the worker is reading the payload.
        // The worker is deliberately not force-killed mid-read; instead its
        // completion is ignored so it cannot resurrect a user-paused or
        // cancelled job. A later resume starts a fresh verification pass.
        if let Some(job_id) = &job_id {
            let job_id_for_db = job_id.clone();
            let state = self
                .run_db("load_pure_v2_recheck_state", move |db| {
                    rt_db::get_job(db, &job_id_for_db)
                        .map(|job| job.state)
                        .map_err(|error| error.to_string())
                })
                .await?;
            if matches!(
                state.as_str(),
                JOB_STATE_PAUSED | JOB_STATE_CANCELLED | JOB_STATE_FAILED
            ) {
                let event = self.session_event_row(
                    Some(&info_hash),
                    "check_discarded",
                    Some("pure v2 file-root recheck was discarded"),
                    serde_json::json!({ "job_id": job_id }),
                );
                self.set_registry_state_with_event(
                    &info_hash,
                    TorrentState::Paused,
                    None,
                    Some(event),
                )
                .await?;
                return Ok(());
            }
            self.persist_pure_v2_recheck_job_async(job_id, done, total_files, &invalid_files)
                .await?;
        }

        let event = self.session_event_row(
            Some(&info_hash),
            "check_completed",
            Some("pure v2 file-root recheck completed"),
            serde_json::json!({
                "total_length": total_length,
                "invalid_files": invalid_files,
            }),
        );
        if invalid_files.is_empty() {
            self.set_registry_state_with_event(
                &info_hash,
                TorrentState::Seeding,
                Some(total_length),
                Some(event),
            )
            .await?;
        } else {
            self.set_registry_state_with_event(&info_hash, TorrentState::Paused, None, Some(event))
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
        self.set_registry_state_with_event(info_hash, state, completed_length, None)
            .await
    }

    async fn set_registry_state_with_event(
        &self,
        info_hash: &str,
        state: TorrentState,
        completed_length: Option<u64>,
        event: Option<rt_db::SessionEventRow>,
    ) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        let mut row = {
            let info_hash = info_hash.to_owned();
            self.run_db("load_torrent_for_state_update", move |db| {
                rt_db::get(db, &info_hash).map_err(|e| e.to_string())
            })
            .await?
        };
        let (previous, was_dormant) = {
            let mut reg = self.registry.write().await;
            let was_dormant = reg.is_dormant(info_hash);
            let mut entry = reg
                .get_mut(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            let previous = entry.clone();
            entry.transition(state).map_err(|error| error.to_string())?;
            if let Some(total) = completed_length {
                entry.total_length = total;
                entry.amount_left = 0;
                if entry.completed_at.is_none() {
                    entry.completed_at = Some(unix_now_i64() as u64);
                }
            }
            row.state = entry.state.as_str().to_owned();
            row.completed_at = entry.completed_at.map(db_i64);
            row.downloaded = db_i64(entry.total_length.saturating_sub(entry.amount_left));
            (previous, was_dormant)
        };
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("persist_torrent_state", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                if let Some(event) = event.as_ref() {
                    rt_db::append_session_event_in_tx(&tx, event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                }
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn update_torrent_labels_inner(
        &self,
        info_hash: &str,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        let mut row = {
            let info_hash = info_hash.to_owned();
            self.run_db("load_torrent_for_label_update", move |db| {
                rt_db::get(db, &info_hash).map_err(|e| e.to_string())
            })
            .await?
        };
        let mut reg = self.registry.write().await;
        let was_dormant = reg.is_dormant(info_hash);
        let mut entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;
        let previous = entry.clone();

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
        // Keep label persistence on the compact durable row. Rebuilding the
        // full metainfo tree here made an otherwise tiny label mutation read
        // and parse an unbounded blob on the engine actor.
        row.category = entry.category.clone();
        row.tags = entry.tags.clone();
        drop(entry);
        drop(reg);

        let labels_event = self.session_event_row(
            Some(info_hash),
            EVENT_LABELS_UPDATED,
            Some("torrent labels updated"),
            serde_json::json!({
                "category": row.category,
                "tags": row.tags,
            }),
        );
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("persist_torrent_labels", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                rt_db::append_session_event_in_tx(&tx, &labels_event)
                    .map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn create_category_inner(&self, name: &str, save_path: Option<&str>) -> CmdResult<()> {
        let name = normalize_category(Some(name.to_owned()))
            .ok_or_else(|| "category name must not be empty".to_owned())?;
        let save_path = normalize_optional_text(save_path.map(str::to_owned));
        self.run_db("create_category", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::create_category_in_tx(&tx, &name, save_path.as_deref(), unix_now_i64())
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await
    }

    async fn rename_category_inner(
        &self,
        old_name: &str,
        new_name: &str,
        save_path: Option<&str>,
    ) -> CmdResult<()> {
        let old_name = normalize_category(Some(old_name.to_owned()))
            .ok_or_else(|| "category name must not be empty".to_owned())?;
        let new_name = normalize_category(Some(new_name.to_owned()))
            .ok_or_else(|| "new category name must not be empty".to_owned())?;
        let save_path = normalize_optional_text(save_path.map(str::to_owned));
        let db_old_name = old_name.clone();
        let db_new_name = new_name.clone();
        self.run_db("rename_category", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::rename_category_in_tx(&tx, &db_old_name, &db_new_name, save_path.as_deref())
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await?;
        self.registry
            .write()
            .await
            .rename_category(&old_name, &new_name);
        Ok(())
    }

    async fn remove_categories_inner(&self, names: &[String]) -> CmdResult<()> {
        let names = names
            .iter()
            .filter_map(|name| normalize_category(Some(name.clone())))
            .collect::<Vec<_>>();
        if names.is_empty() {
            return Ok(());
        }
        let names_for_db = names.clone();
        self.run_db("remove_categories", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::remove_categories_in_tx(&tx, &names_for_db)
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await?;
        self.registry.write().await.clear_categories(&names);
        Ok(())
    }

    async fn create_tags_inner(&self, names: &[String]) -> CmdResult<()> {
        let names = normalize_tags(names.to_vec());
        if names.is_empty() {
            return Ok(());
        }
        self.run_db("create_tags", move |db| {
            let mut tags = persisted_global_tags(db)?
                .into_iter()
                .collect::<BTreeSet<_>>();
            tags.extend(names);
            let value = serde_json::to_string(&tags.into_iter().collect::<Vec<_>>())
                .map_err(|error| error.to_string())?;
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::set_setting_in_tx(&tx, SETTING_GLOBAL_TAGS, &value, unix_now_i64())
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await
    }

    async fn remove_tags_inner(&self, names: &[String]) -> CmdResult<()> {
        let names = normalize_tags(names.to_vec());
        if names.is_empty() {
            return Ok(());
        }
        let remove = names.iter().cloned().collect::<HashSet<_>>();
        let names_for_db = names.clone();
        self.run_db("remove_tags", move |db| {
            let mut definitions = persisted_global_tags(db)?
                .into_iter()
                .collect::<BTreeSet<_>>();
            for name in &names_for_db {
                definitions.remove(name);
            }
            let rows = rt_db::list_all(db).map_err(|error| error.to_string())?;
            let value = serde_json::to_string(&definitions.into_iter().collect::<Vec<_>>())
                .map_err(|error| error.to_string())?;
            let tx = db.transaction().map_err(|error| error.to_string())?;
            for mut row in rows {
                let before = row.tags.len();
                row.tags.retain(|tag| !remove.contains(tag.as_str()));
                if row.tags.len() != before {
                    rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                }
            }
            rt_db::set_setting_in_tx(&tx, SETTING_GLOBAL_TAGS, &value, unix_now_i64())
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await?;

        // Keep the in-memory projection in lockstep with the durable update,
        // including compact dormant records without promoting them.
        self.registry.write().await.clear_tags(&names);
        Ok(())
    }

    /// Begin a mutable-field update without making the engine actor perform
    /// per-file filesystem admission. A save-path move needs `exists()` and
    /// device detection for each persisted file; those calls belong on the
    /// blocking storage-planning task. The actor receives a prepared plan
    /// later and remains available for health, lifecycle, and peer commands in
    /// the meantime.
    async fn begin_update_torrent_fields(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<PathBuf>,
        reply: oneshot::Sender<CmdResult<Option<String>>>,
    ) {
        if let Err(error) = self.ensure_torrent_jobs_idle(&info_hash).await {
            let _ = reply.send(Err(error));
            return;
        }
        let normalized_name = normalize_optional_text(name);
        let (current_name, current_save_path) = {
            let reg = self.registry.read().await;
            let Some(entry) = reg.get(&info_hash) else {
                let _ = reply.send(Err(format!("torrent {info_hash} not found")));
                return;
            };
            (entry.name.clone(), PathBuf::from(&entry.save_path))
        };

        let Some(target_save_path) = save_path else {
            let result = self
                .persist_torrent_fields_inner(&info_hash, normalized_name, None)
                .await;
            let _ = reply.send(result);
            return;
        };
        if target_save_path == current_save_path {
            let result = self
                .persist_torrent_fields_inner(&info_hash, normalized_name, Some(target_save_path))
                .await;
            let _ = reply.send(result);
            return;
        }

        // Root configuration is a small, bounded control-plane read. The
        // expensive per-file path probing and same-device decision are moved
        // below the actor boundary.
        let authority = match self.configured_storage_authority_async().await {
            Ok(authority) => authority,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let file_entries = match self.torrent_file_entries_async(&info_hash).await {
            Ok(file_entries) => file_entries,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let planning_source = current_save_path.clone();
        let planning_destination = target_save_path.clone();
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let plan_result = match tokio::task::spawn_blocking(move || {
                plan_torrent_payload_files_with_authority(
                    &authority,
                    &planning_source,
                    &planning_destination,
                    &file_entries,
                )
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(format!("storage move planning task failed: {error}")),
            };
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::PreparedTorrentFields {
                    info_hash,
                    name: normalized_name,
                    current_name,
                    current_save_path,
                    save_path: target_save_path,
                    plan: plan_result,
                    reply,
                }),
            )
            .await;
        });
    }

    async fn finish_prepared_torrent_fields(
        &self,
        info_hash: &str,
        normalized_name: Option<String>,
        current_name: &str,
        current_save_path: &Path,
        target_save_path: PathBuf,
        plan: CmdResult<Option<StoragePlan>>,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let current = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            (entry.name.clone(), PathBuf::from(&entry.save_path))
        };
        if current.0 != current_name || current.1 != current_save_path {
            return Err(format!(
                "torrent {info_hash} changed while its storage move was being planned; retry"
            ));
        }

        let Some(plan) = plan? else {
            return self
                .persist_torrent_fields_inner(info_hash, normalized_name, Some(target_save_path))
                .await;
        };
        self.queue_torrent_move_after_plan(
            info_hash,
            normalized_name,
            current_save_path.to_path_buf(),
            target_save_path,
            plan,
        )
        .await
    }

    async fn queue_torrent_move_after_plan(
        &self,
        info_hash: &str,
        normalized_name: Option<String>,
        current_save_path: PathBuf,
        target_save_path: PathBuf,
        plan: StoragePlan,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let quiesced = self
            .quiesce_torrent_for_storage_move(info_hash)
            .await
            .map_err(|error| {
                format!("torrent {info_hash} could not be quiesced for storage move: {error}")
            })?;
        let (completion, completion_rx) = oneshot::channel();
        let result = self
            .queue_storage_plan_job_with_context(
                "move",
                vec![info_hash.to_owned()],
                &plan,
                Vec::new(),
                serde_json::json!({
                    "old_save_path": current_save_path.display().to_string(),
                    "save_path": target_save_path.display().to_string(),
                    "name": normalized_name.clone(),
                }),
                completion,
            )
            .await;
        if let Ok(job_id) = &result {
            let cmd_tx = self.cmd_tx.clone();
            let job_id_for_task = job_id.clone();
            let info_hash = info_hash.to_owned();
            tokio::spawn(async move {
                let completion = completion_rx.await.unwrap_or_else(|_| {
                    StorageJobCompletion::failed(
                        "storage worker completion channel closed",
                        Vec::new(),
                    )
                });
                let _ = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    cmd_tx.send(EngineCmd::StorageMoveFinished {
                        job_id: job_id_for_task,
                        info_hash,
                        name: normalized_name,
                        old_save_path: current_save_path,
                        save_path: target_save_path,
                        quiesced,
                        succeeded: completion.succeeded,
                        terminal_state: completion.state,
                        error: completion.error,
                        completed_steps: completion.completed_steps,
                        retry_attempt: 0,
                    }),
                )
                .await;
            });
            return Ok(Some(job_id.clone()));
        }
        self.resume_torrent_after_storage_move(info_hash, quiesced, None)
            .await;
        result.map(|_| None)
    }

    #[cfg(test)]
    async fn update_torrent_fields_inner(
        &self,
        info_hash: &str,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
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
            self.authorize_storage_path_async(target).await?;
        }
        if let Some(target) = &target_save_path {
            if *target != current_save_path {
                if let Some(plan) = self.plan_torrent_payload_files(
                    &current_save_path,
                    target,
                    &self.torrent_file_entries(info_hash)?,
                )? {
                    // The actor only performs bounded orchestration here.
                    // Filesystem work and checkpoints run behind the storage
                    // worker boundary; completion comes back as a command so
                    // health/lifecycle requests remain serviceable.
                    let quiesced = self
                        .quiesce_torrent_for_storage_move(info_hash)
                        .await
                        .map_err(|error| {
                            format!(
                                "torrent {info_hash} could not be quiesced for storage move: {error}"
                            )
                        })?;
                    let (completion, completion_rx) = oneshot::channel();
                    let result = self
                        .queue_storage_plan_job_with_context(
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
                        )
                        .await;
                    if let Ok(job_id) = &result {
                        let cmd_tx = self.cmd_tx.clone();
                        let job_id_for_task = job_id.clone();
                        let info_hash = info_hash.to_owned();
                        let name = normalized_name.clone();
                        let old_save_path = current_save_path.clone();
                        let save_path = target.clone();
                        tokio::spawn(async move {
                            let completion = completion_rx.await.unwrap_or_else(|_| {
                                StorageJobCompletion::failed(
                                    "storage worker completion channel closed",
                                    Vec::new(),
                                )
                            });
                            let _ = timeout(
                                ENGINE_COMMAND_SEND_TIMEOUT,
                                cmd_tx.send(EngineCmd::StorageMoveFinished {
                                    job_id: job_id_for_task,
                                    info_hash,
                                    name,
                                    old_save_path,
                                    save_path,
                                    quiesced,
                                    succeeded: completion.succeeded,
                                    terminal_state: completion.state,
                                    error: completion.error,
                                    completed_steps: completion.completed_steps,
                                    retry_attempt: 0,
                                }),
                            )
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
        let was_dormant = reg.is_dormant(info_hash);
        let mut entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;
        let previous = entry.clone();

        if let Some(name) = normalized_name {
            entry.name = name;
        }
        if let Some(save_path) = target_save_path {
            entry.save_path = save_path.to_string_lossy().to_string();
        }

        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
            row.name = entry.name.clone();
            row.save_path = entry.save_path.clone();
            row
        };
        drop(entry);
        drop(reg);

        let fields_event = self.session_event_row(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": row.name,
                "save_path": row.save_path,
            }),
        );
        let persistence = (|| -> Result<(), String> {
            let mut db = self.db.lock().expect("database mutex poisoned");
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &fields_event)
                .map_err(|error| error.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, self.config.logging.event_retention)
                .map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })();
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(None)
    }

    async fn persist_torrent_fields_inner(
        &self,
        info_hash: &str,
        normalized_name: Option<String>,
        target_save_path: Option<PathBuf>,
    ) -> CmdResult<Option<String>> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        let mut row = {
            let info_hash = info_hash.to_owned();
            self.run_db("load_torrent_for_field_update", move |db| {
                rt_db::get(db, &info_hash).map_err(|e| e.to_string())
            })
            .await?
        };
        let mut reg = self.registry.write().await;
        let was_dormant = reg.is_dormant(info_hash);
        let mut entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;
        let previous = entry.clone();

        if let Some(name) = normalized_name {
            entry.name = name;
        }
        if let Some(save_path) = target_save_path {
            entry.save_path = save_path.to_string_lossy().to_string();
        }

        row.name = entry.name.clone();
        row.save_path = entry.save_path.clone();
        drop(entry);
        drop(reg);

        let fields_event = self.session_event_row(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": row.name,
                "save_path": row.save_path,
            }),
        );
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("persist_torrent_fields", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                rt_db::append_session_event_in_tx(&tx, &fields_event)
                    .map_err(|error| error.to_string())?;
                rt_db::prune_session_events_in_tx(&tx, retention)
                    .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(None)
    }

    #[cfg(test)]
    fn plan_torrent_payload_files(
        &self,
        source_root: &std::path::Path,
        destination_root: &std::path::Path,
        file_entries: &[(rt_path::SafeRelPath, u64)],
    ) -> CmdResult<Option<StoragePlan>> {
        self.authorize_storage_path(source_root)?;
        self.authorize_storage_path(destination_root)?;
        let mut steps = Vec::new();
        let mut rollback_steps = Vec::new();
        let mut issues = Vec::new();
        for (rel_path, bytes) in file_entries.iter() {
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
                bytes: *bytes,
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

    #[cfg(test)]
    fn torrent_file_entries(&self, info_hash: &str) -> CmdResult<Vec<(rt_path::SafeRelPath, u64)>> {
        let file_rows = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_torrent_files(&db, info_hash).map_err(|error| error.to_string())?
        };
        file_entries_from_rows(&file_rows)
    }

    async fn torrent_file_entries_async(
        &self,
        info_hash: &str,
    ) -> CmdResult<Vec<(rt_path::SafeRelPath, u64)>> {
        let info_hash_for_db = info_hash.to_owned();
        let file_rows = self
            .run_db("list_torrent_files_for_storage_plan", move |db| {
                rt_db::list_torrent_files(db, &info_hash_for_db).map_err(|error| error.to_string())
            })
            .await?;
        file_entries_from_rows(&file_rows)
    }

    async fn plan_torrent_payload_delete(
        &self,
        save_root: &Path,
        file_entries: &[(rt_path::SafeRelPath, u64)],
    ) -> CmdResult<Option<StoragePlan>> {
        self.authorize_storage_path_async(save_root).await?;
        if file_entries.is_empty() {
            return Ok(None);
        }

        let mut steps = Vec::with_capacity(file_entries.len());
        let mut prune_dirs = HashSet::new();
        for (rel_path, bytes) in file_entries.iter() {
            let path = rel_path.resolve(save_root);
            // The worker repeats root confinement immediately before
            // execution. This admission check catches a bad configured path
            // before a job is made visible, without stat-ing every payload
            // file on the actor thread.
            if !path.starts_with(save_root) {
                return Err(format!(
                    "torrent payload path escapes save root: {}",
                    path.display()
                ));
            }
            steps.push(StoragePlanStep {
                action: rt_storage::PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(path.clone()),
                destination: None,
                bytes: *bytes,
            });

            let mut parent = path.parent();
            while let Some(dir) = parent {
                if dir == save_root || !dir.starts_with(save_root) {
                    break;
                }
                prune_dirs.insert(dir.to_path_buf());
                parent = dir.parent();
            }
        }

        // All file removals must precede directory pruning. Each prune step
        // stops at the first non-empty directory, preserving files belonging
        // to another torrent or operator-managed content.
        let mut prune_dirs = prune_dirs.into_iter().collect::<Vec<_>>();
        prune_dirs.sort_by_key(|left| std::cmp::Reverse(left.components().count()));
        steps.extend(prune_dirs.into_iter().map(|dir| StoragePlanStep {
            action: rt_storage::PlannedStorageAction::PruneEmptyDirs,
            source: Some(dir),
            destination: Some(save_root.to_path_buf()),
            bytes: 0,
        }));

        Ok(Some(StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps,
            rollback_steps: Vec::new(),
        }))
    }

    /// Quiesces the running task for `info_hash` before a storage move, if
    /// one exists. Returns `Some(was_already_paused)` when a task was
    /// quiesced (the caller must resume it afterward via
    /// `resume_torrent_after_storage_move`), or `None` when there is no
    /// running task -- nothing to quiesce, and correspondingly nothing to
    /// resume. A live task that cannot acknowledge quiescence is an error;
    /// storage work must never proceed in that case.
    async fn quiesce_torrent_for_storage_move(&self, info_hash: &str) -> CmdResult<Option<bool>> {
        let Some(tx) = self.runtime.torrent_chans.get(info_hash).cloned() else {
            return Ok(None);
        };
        Ok(Some(quiesce_torrent_channel(&tx).await?))
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
        if let Some(tx) = self.runtime.torrent_chans.get(info_hash).cloned() {
            let _ = send_torrent_command(
                &tx,
                TorrentCmd::ResumeAfterStorageMove {
                    new_save_root,
                    resume_paused: was_paused,
                },
            )
            .await;
        }
    }

    /// Quiesces every torrent in `info_hashes` that currently has a
    /// running task, for the duration of a generic (non-save-path-owning)
    /// storage plan execution. Returns the `(info_hash, was_already_paused)`
    /// pairs that actually got quiesced, for `resume_torrents_after_storage_plan`.
    /// If any live task fails to acknowledge, already-quiesced tasks are
    /// resumed and the plan is rejected before the worker can touch files.
    async fn quiesce_torrents_for_storage_plan(
        &self,
        info_hashes: &[String],
    ) -> CmdResult<Vec<(String, bool)>> {
        let targets = info_hashes
            .iter()
            .filter_map(|info_hash| {
                self.runtime
                    .torrent_chans
                    .get(info_hash)
                    .cloned()
                    .map(|tx| (info_hash.clone(), tx))
            })
            .collect::<Vec<_>>();
        let results: Vec<(String, CmdResult<bool>)> =
            stream::iter(targets.into_iter().map(|(info_hash, tx)| async move {
                let result = quiesce_torrent_channel(&tx).await;
                (info_hash, result)
            }))
            .buffer_unordered(TIER_IDLE_RECONCILE_CONCURRENCY)
            .collect()
            .await;
        let mut quiesced = Vec::with_capacity(results.len());
        for (info_hash, result) in results {
            match result {
                Ok(was_paused) => quiesced.push((info_hash, was_paused)),
                Err(error) => {
                    self.resume_torrents_after_storage_plan(quiesced).await;
                    return Err(format!(
                        "torrent {info_hash} could not be quiesced: {error}"
                    ));
                }
            }
        }
        Ok(quiesced)
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

    async fn complete_storage_plan_job_async(
        &self,
        job_id: &str,
        completed_steps: &[usize],
    ) -> Result<(), String> {
        let job_id_for_db = job_id.to_owned();
        let mut job = self
            .run_db("load_storage_plan_completion", move |db| {
                rt_db::get_job(db, &job_id_for_db).map_err(|error| error.to_string())
            })
            .await?;
        if job.kind != JOB_KIND_STORAGE_PLAN {
            return Err(format!("job {job_id} is not a storage plan"));
        }
        if job.state == JOB_STATE_COMPLETED && job.finished_at.is_some() {
            return Ok(());
        }
        let now = unix_now_i64();
        job.state = JOB_STATE_COMPLETED.to_owned();
        job.error = None;
        job.done = db_i64_usize(completed_steps.len());
        job.checkpoint = job.done;
        job.file_index = Some(job.done);
        job.updated_at = now;
        job.finished_at = Some(now);
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: "storage_plan_completed".to_owned(),
            message: Some("storage plan actor-side commit completed".to_owned()),
            payload: serde_json::json!({
                "state": JOB_STATE_COMPLETED,
                "completed_steps": completed_steps,
            })
            .to_string(),
        };
        self.persist_job_with_events_async(&job, &[event]).await
    }

    /// Complete a payload-delete job. The registry/database projection stays
    /// addressable until the worker reports a successful delete, so failed or
    /// cancelled cleanup can resume the torrent and be retried. The same
    /// finalizer handles a crash/restart where the durable job finishes
    /// against a restored torrent row.
    async fn finish_storage_delete(
        &mut self,
        completion: StorageDeleteCompletion,
    ) -> CmdResult<()> {
        let StorageDeleteCompletion {
            job_id,
            info_hash,
            succeeded,
            terminal_state,
            error,
            completed_steps,
            quiesced,
        } = completion;
        // Shutdown requeues the work. Keep the task quiesced and the
        // projection visible; restart recovery will reattach the job and
        // complete or cancel it. User cancellation/failure, in contrast,
        // releases the quiesce so the payload remains usable.
        if terminal_state == JOB_STATE_QUEUED {
            return Ok(());
        }
        if !succeeded || terminal_state != JOB_STATE_COMPLETED {
            self.runtime.pending_torrent_deletes.remove(&info_hash);
            self.resume_torrents_after_storage_plan(quiesced).await;
            self.append_session_event(
                Some(&info_hash),
                EVENT_TORRENT_REMOVE_FAILED,
                Some("torrent payload cleanup did not complete"),
                serde_json::json!({
                    "job_id": job_id,
                    "payload_cleanup": "failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                }),
            );
            return Ok(());
        }

        self.runtime.pending_torrent_deletes.remove(&info_hash);

        // Stop the quiesced task before deleting its metadata. A task restored
        // in the crash window is handled by this same path.
        self.stop_torrent_task(&info_hash).await;
        self.runtime.tier_controller.remove(&info_hash);
        self.runtime.tier_last_active.remove(&info_hash);

        // Keep the restored registry row visible if bounded metadata cleanup
        // fails. A caller/operator can then retry the durable job instead of
        // getting a silently split registry/DB projection.
        let removal_event = self.session_event_row(
            Some(&info_hash),
            EVENT_TORRENT_REMOVED,
            Some("torrent removed after recovered payload cleanup"),
            serde_json::json!({
                "delete_files": true,
                "payload_delete_job_id": job_id,
                "recovered": true,
            }),
        );
        if let Err(error) = self
            .delete_persisted_torrent(&info_hash, Some(&removal_event))
            .await
        {
            self.update_job_state_async(
                &job_id,
                JOB_STATE_FAILED,
                Some(format!(
                    "payload deleted but metadata cleanup failed: {error}"
                )),
                Some("payload delete metadata cleanup failed"),
            )
            .await?;
            return Err(error.to_string());
        }

        let _ = self.registry.write().await.remove(&info_hash);
        self.unregister_dht_torrent(&info_hash).await;
        Ok(())
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
        terminal_state: String,
        error: Option<String>,
        completed_steps: Vec<usize>,
        retry_attempt: u8,
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
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                }),
            );
            return Ok(());
        }

        let retry_name = name.clone();
        let entry = {
            let mut registry = self.registry.write().await;
            let Some(mut entry) = registry.get_mut(info_hash) else {
                drop(registry);
                self.resume_torrent_after_storage_move(info_hash, quiesced, None)
                    .await;
                return Err(format!(
                    "torrent {info_hash} disappeared during storage move"
                ));
            };
            if let Some(name) = name {
                entry.name = name;
            }
            entry.save_path = save_path.to_string_lossy().to_string();
            entry.clone()
        };
        let info_hash_for_db = info_hash.to_owned();
        let persisted_row = self
            .run_db("load_torrent_for_storage_move", move |db| {
                rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())
            })
            .await;
        let mut row = match persisted_row {
            Ok(row) => row,
            Err(error) => {
                // The filesystem move has already committed. A transient DB
                // read failure must not resume a live task against the old
                // root, which may no longer contain the payload. Keep the
                // destination live and retry the actor-side commit.
                self.resume_torrent_after_storage_move(
                    info_hash,
                    quiesced,
                    Some(save_path.clone()),
                )
                .await;
                let error = error.to_string();
                self.schedule_storage_move_commit_retry(
                    job_id,
                    info_hash,
                    retry_name,
                    old_save_path.clone(),
                    save_path.clone(),
                    quiesced,
                    completed_steps.clone(),
                    retry_attempt,
                );
                self.append_session_event(
                    Some(info_hash),
                    EVENT_FIELDS_UPDATED,
                    Some("storage move committed on disk but durable save path read failed"),
                    serde_json::json!({
                        "job_id": job_id,
                        "old_save_path": old_save_path,
                        "save_path": save_path,
                        "storage_move": "filesystem_committed_db_read_failed",
                        "terminal_state": terminal_state,
                        "error": error,
                        "completed_steps": completed_steps,
                        "retry_attempt": retry_attempt,
                    }),
                );
                return Err(error);
            }
        };
        row.name = entry.name.clone();
        row.save_path = entry.save_path.clone();
        let row_for_db = row.clone();
        let persistence_error = self
            .run_db("persist_storage_move_projection", move |db| {
                rt_db::upsert(db, &row_for_db).map_err(|error| error.to_string())
            })
            .await
            .err();
        if let Some(error) = persistence_error {
            // The worker's filesystem transaction is already committed and
            // the durable job remains `commit_pending`. Keeping the live
            // projection on the destination is safer than resuming a task
            // against the now-missing old path; restart recovery will retry
            // the database projection commit from the durable plan context.
            self.resume_torrent_after_storage_move(info_hash, quiesced, Some(save_path.clone()))
                .await;
            self.schedule_storage_move_commit_retry(
                job_id,
                info_hash,
                retry_name,
                old_save_path.clone(),
                save_path.clone(),
                quiesced,
                completed_steps.clone(),
                retry_attempt,
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move completed on disk but durable save path persistence failed"),
                serde_json::json!({
                    "job_id": job_id,
                    "old_save_path": old_save_path,
                    "save_path": save_path,
                    "storage_move": "filesystem_committed_db_failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                    "retry_attempt": retry_attempt,
                }),
            );
            return Err(error);
        }
        self.resume_torrent_after_storage_move(info_hash, quiesced, Some(save_path.clone()))
            .await;
        if let Err(error) = self
            .complete_storage_plan_job_async(job_id, &completed_steps)
            .await
        {
            self.schedule_storage_move_commit_retry(
                job_id,
                info_hash,
                retry_name,
                old_save_path.clone(),
                save_path.clone(),
                quiesced,
                completed_steps.clone(),
                retry_attempt,
            );
            self.append_session_event(
                Some(info_hash),
                EVENT_FIELDS_UPDATED,
                Some("storage move committed but durable job completion failed"),
                serde_json::json!({
                    "job_id": job_id,
                    "save_path": save_path,
                    "storage_move": "committed_job_completion_failed",
                    "terminal_state": terminal_state,
                    "error": error,
                    "completed_steps": completed_steps,
                    "retry_attempt": retry_attempt,
                }),
            );
            return Err(error);
        }
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

    #[allow(clippy::too_many_arguments)]
    fn schedule_storage_move_commit_retry(
        &self,
        job_id: &str,
        info_hash: &str,
        name: Option<String>,
        old_save_path: PathBuf,
        save_path: PathBuf,
        quiesced: Option<bool>,
        completed_steps: Vec<usize>,
        retry_attempt: u8,
    ) {
        if retry_attempt >= STORAGE_MOVE_COMMIT_MAX_RETRIES {
            warn!(
                component = "storage_jobs",
                operation = "retry_storage_move_commit",
                job_id,
                torrent = %info_hash,
                retry_attempt,
                result = "exhausted",
                "storage move actor-side commit retries exhausted; job remains commit_pending"
            );
            return;
        }
        let exponent = u32::from(retry_attempt).min(4);
        let delay = STORAGE_MOVE_COMMIT_RETRY_BASE
            .checked_mul(1_u32 << exponent)
            .unwrap_or(Duration::from_secs(5));
        let cmd_tx = self.cmd_tx.clone();
        let job_id = job_id.to_owned();
        let info_hash = info_hash.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = timeout(
                ENGINE_COMMAND_SEND_TIMEOUT,
                cmd_tx.send(EngineCmd::StorageMoveFinished {
                    job_id,
                    info_hash,
                    name,
                    old_save_path,
                    save_path,
                    quiesced,
                    succeeded: true,
                    terminal_state: STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                    error: None,
                    completed_steps,
                    retry_attempt: retry_attempt.saturating_add(1),
                }),
            )
            .await;
        });
    }

    async fn update_torrent_trackers_inner(
        &self,
        info_hash: &str,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        let trackers = normalize_tracker_urls(trackers);
        let mut row = {
            let info_hash = info_hash.to_owned();
            self.run_db("load_torrent_for_tracker_update", move |db| {
                rt_db::get(db, &info_hash).map_err(|e| e.to_string())
            })
            .await?
        };
        row.trackers = trackers.clone();
        let tracker_rows = tracker_rows_from_urls(
            info_hash,
            &trackers,
            row.uploaded,
            row.downloaded,
            row.total_length.saturating_sub(row.downloaded).max(0),
        );
        let trackers_event = self.session_event_row(
            Some(info_hash),
            EVENT_TRACKERS_UPDATED,
            Some("torrent trackers updated"),
            serde_json::json!({ "trackers": trackers }),
        );

        let info_hash_owned = info_hash.to_owned();
        let retention = self.config.logging.event_retention;
        self.run_db("persist_torrent_trackers", move |db| {
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::upsert_in_tx(&tx, &row).map_err(|e| e.to_string())?;
            rt_db::replace_torrent_trackers_in_tx(&tx, &info_hash_owned, &tracker_rows)
                .map_err(|e| e.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &trackers_event).map_err(|e| e.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await?;
        Ok(())
    }

    async fn torrent_limits_inner(&self, info_hash: &str) -> CmdResult<EngineTorrentLimits> {
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        let info_hash = info_hash.to_owned();
        self.run_db(
            "get_torrent_limits",
            move |db| match rt_db::get_torrent_limits(db, &info_hash) {
                Ok(row) => Ok(engine_limits_from_row(row)),
                Err(rt_db::DbError::NotFound(_)) => Ok(EngineTorrentLimits::default()),
                Err(e) => Err(e.to_string()),
            },
        )
        .await
    }

    async fn update_torrent_limits_inner(
        &self,
        info_hash: &str,
        limits: EngineTorrentLimits,
    ) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
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
        let limits_event = self.session_event_row(
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
        let retention = self.config.logging.event_retention;
        self.run_db("update_torrent_limits", move |db| {
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::upsert_torrent_limits_in_tx(&tx, &row).map_err(|e| e.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &limits_event).map_err(|e| e.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await?;
        if self.runtime.torrent_chans.contains_key(info_hash) {
            // The durable row is authoritative, but a running task must also
            // observe the new limits now. A full/closed task mailbox is a
            // failed mutation, not a successful eventual update; report it
            // to the caller so the API cannot claim runtime state changed.
            let (reply, response) = oneshot::channel();
            self.send_to_torrent(
                info_hash,
                TorrentCmd::UpdateLimits {
                    limits,
                    reply: Some(reply),
                },
            )
            .await?;
            await_engine_reply(response).await?;
        }
        Ok(())
    }

    async fn update_file_priorities_inner(
        &self,
        info_hash: &str,
        file_ids: Vec<u32>,
        priority: i64,
    ) -> CmdResult<()> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
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
        let mut event_file_ids = ids.iter().copied().collect::<Vec<_>>();
        event_file_ids.sort_unstable();
        let priorities_event = self.session_event_row(
            Some(info_hash),
            "file_priorities_updated",
            Some("torrent file priorities updated"),
            serde_json::json!({
                "file_ids": event_file_ids,
                "priority": priority,
                "wanted": wanted,
            }),
        );
        let info_hash_owned = info_hash.to_owned();
        let retention = self.config.logging.event_retention;
        self.run_db("update_file_priorities", move |db| {
            let mut files =
                rt_db::list_torrent_files(db, &info_hash_owned).map_err(|e| e.to_string())?;
            if files.is_empty() {
                return Err(format!("torrent {info_hash_owned} has no persisted files"));
            }
            let apply_all = ids.is_empty();
            let mut touched = 0usize;
            for file in &mut files {
                if apply_all
                    || u32::try_from(file.file_index)
                        .ok()
                        .is_some_and(|file_id| ids.contains(&file_id))
                {
                    file.priority = priority;
                    file.wanted = wanted;
                    touched += 1;
                }
            }
            if touched == 0 {
                return Err(format!("no matching files for torrent {info_hash_owned}"));
            }
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::replace_torrent_files_in_tx(&tx, &info_hash_owned, &files)
                .map_err(|e| e.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &priorities_event).map_err(|e| e.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await?;
        if self.runtime.torrent_chans.contains_key(info_hash) {
            let (reply, response) = oneshot::channel();
            self.send_to_torrent(
                info_hash,
                TorrentCmd::ReloadFilePolicy { reply: Some(reply) },
            )
            .await?;
            await_engine_reply(response).await?;
        }
        Ok(())
    }

    async fn begin_add_peers(
        &mut self,
        info_hash: String,
        peers: Vec<SocketAddr>,
        reply: oneshot::Sender<CmdResult<()>>,
    ) {
        if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
            let _ = reply.send(Err(error));
            return;
        }
        if peers.is_empty() {
            let _ = reply.send(Ok(()));
            return;
        }
        {
            let reg = self.registry.read().await;
            if reg.get(&info_hash).is_none() {
                let _ = reply.send(Err(format!("torrent {info_hash} not found")));
                return;
            }
        }
        let placeholder = match self.metadata_placeholder_row_checked(&info_hash).await {
            Ok(row) => row,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let taskless_v2 = !self.runtime.torrent_chans.contains_key(&info_hash)
            && placeholder.is_none()
            && self.is_pure_v2_torrent(&info_hash);
        if taskless_v2 {
            let _ = reply.send(Err("pure v2 peer transfer is not implemented".to_owned()));
            return;
        }
        if placeholder.is_some() {
            let result = match self.ensure_metadata_task(&info_hash).await {
                Ok(()) => match self.send_to_torrent(&info_hash, TorrentCmd::Resume).await {
                    Ok(()) => {
                        self.send_to_torrent(&info_hash, TorrentCmd::PriorityPeers(peers))
                            .await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            let _ = reply.send(result);
            return;
        }

        match self.begin_torrent_task_promotion(
            &info_hash,
            TorrentPromotionAction::AddPeers { peers, reply },
        ) {
            TorrentPromotionBegin::Ready(action) => {
                self.execute_torrent_promotion_action(&info_hash, *action, false)
                    .await;
            }
            TorrentPromotionBegin::Pending => {}
        }
    }

    #[cfg(test)]
    async fn add_peers_inner(&mut self, info_hash: &str, peers: Vec<SocketAddr>) -> CmdResult<()> {
        self.ensure_torrent_storage_idle(info_hash).await?;
        if peers.is_empty() {
            return Ok(());
        }
        {
            let reg = self.registry.read().await;
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        let placeholder = self.metadata_placeholder_row_checked_sync(info_hash)?;
        let taskless_v2 = !self.runtime.torrent_chans.contains_key(info_hash)
            && placeholder.is_none()
            && self.is_pure_v2_torrent(info_hash);
        if taskless_v2 {
            return Err("pure v2 peer transfer is not implemented".to_owned());
        }
        let was_taskless = !self.runtime.torrent_chans.contains_key(info_hash);
        self.ensure_torrent_task(info_hash).await?;
        if was_taskless {
            self.send_to_torrent(info_hash, TorrentCmd::Resume).await?;
        }
        self.send_to_torrent(info_hash, TorrentCmd::PriorityPeers(peers))
            .await
    }

    async fn torrent_peers_inner(&self, info_hash: &str) -> CmdResult<Vec<EnginePeerSnapshot>> {
        let tx = self.runtime.torrent_chans.get(info_hash).cloned();
        let Some(tx) = tx else {
            let reg = self.registry.read().await;
            return if reg.get(info_hash).is_some() {
                Ok(Vec::new())
            } else {
                Err(format!("torrent {info_hash} not found"))
            };
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        send_torrent_command(&tx, TorrentCmd::GetPeers { reply }).await?;
        timeout(ENGINE_COMMAND_REPLY_TIMEOUT, rx)
            .await
            .map_err(|_| "torrent peer query timed out".to_owned())?
            .map_err(|_| "torrent task dropped reply".to_owned())
    }

    async fn rename_file_path_inner(
        &self,
        info_hash: &str,
        file_id: u32,
        new_path: String,
    ) -> CmdResult<()> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        self.ensure_torrent_exists(info_hash).await?;
        let new_path = normalize_relative_path(&new_path)?;
        let path_event = self.session_event_row(
            Some(info_hash),
            "file_path_renamed",
            Some("torrent file path renamed"),
            serde_json::json!({ "file_id": file_id, "new_path": new_path }),
        );
        let info_hash_owned = info_hash.to_owned();
        let retention = self.config.logging.event_retention;
        self.run_db("rename_file_path", move |db| {
            let mut files =
                rt_db::list_torrent_files(db, &info_hash_owned).map_err(|e| e.to_string())?;
            if files.is_empty() {
                return Err(format!("torrent {info_hash_owned} has no persisted files"));
            }
            let Some(file) = files
                .iter_mut()
                .find(|file| u32::try_from(file.file_index).ok() == Some(file_id))
            else {
                return Err(format!(
                    "file {file_id} not found for torrent {info_hash_owned}"
                ));
            };
            file.path = new_path;
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::replace_torrent_files_in_tx(&tx, &info_hash_owned, &files)
                .map_err(|e| e.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &path_event).map_err(|e| e.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await
    }

    async fn rename_folder_path_inner(
        &self,
        info_hash: &str,
        old_path: String,
        new_path: String,
    ) -> CmdResult<()> {
        self.ensure_torrent_jobs_idle(info_hash).await?;
        self.ensure_torrent_exists(info_hash).await?;
        let old_path = normalize_relative_path(&old_path)?;
        let new_path = normalize_relative_path(&new_path)?;
        let old_prefix = format!("{old_path}/");
        let path_event = self.session_event_row(
            Some(info_hash),
            "folder_path_renamed",
            Some("torrent folder path renamed"),
            serde_json::json!({ "old_path": old_path, "new_path": new_path }),
        );
        let info_hash_owned = info_hash.to_owned();
        let retention = self.config.logging.event_retention;
        self.run_db("rename_folder_path", move |db| {
            let mut files =
                rt_db::list_torrent_files(db, &info_hash_owned).map_err(|e| e.to_string())?;
            if files.is_empty() {
                return Err(format!("torrent {info_hash_owned} has no persisted files"));
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
                    "folder {old_path} not found for torrent {info_hash_owned}"
                ));
            }
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::replace_torrent_files_in_tx(&tx, &info_hash_owned, &files)
                .map_err(|e| e.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &path_event).map_err(|e| e.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await
    }

    async fn ensure_torrent_exists(&self, info_hash: &str) -> CmdResult<()> {
        let reg = self.registry.read().await;
        if reg.get(info_hash).is_some() {
            Ok(())
        } else {
            Err(format!("torrent {info_hash} not found"))
        }
    }

    /// Generic storage plans have no reliable way to infer which torrent owns
    /// an arbitrary filesystem path. Move/delete therefore require explicit
    /// torrent targets, and every supplied target must resolve in the current
    /// registry before any task is quiesced or worker is queued.
    async fn validate_storage_plan_targets(
        &self,
        operation: &str,
        info_hashes: &[String],
    ) -> CmdResult<()> {
        let operation = operation.trim().to_ascii_lowercase();
        if !matches!(operation.as_str(), "move" | "import" | "delete") {
            return Err("storage operation must be one of move, import, or delete".to_owned());
        }
        if matches!(operation.as_str(), "move" | "delete") && info_hashes.is_empty() {
            return Err(format!(
                "storage {operation} plans require at least one affected_torrents target"
            ));
        }
        if operation == "move" && info_hashes.len() != 1 {
            return Err(
                "storage move plans require exactly one affected_torrents target".to_owned(),
            );
        }
        if info_hashes.is_empty() {
            return Ok(());
        }
        let reg = self.registry.read().await;
        for info_hash in info_hashes {
            if reg.get(info_hash).is_none() {
                return Err(format!("torrent {info_hash} not found"));
            }
        }
        Ok(())
    }

    /// Resolve the actor-side context for the generic storage API's move
    /// operation. A raw filesystem plan is not enough to update a torrent's
    /// durable save path after the worker commits; the source must be the
    /// current torrent root and the final rename destination becomes the new
    /// root. Reject custom/ambiguous plans rather than completing a move with
    /// a stale torrent projection.
    async fn storage_plan_move_context(
        &self,
        affected_torrents: &[String],
        plan: &StoragePlan,
    ) -> CmdResult<(String, Option<String>, PathBuf, PathBuf)> {
        let info_hash = affected_torrents
            .first()
            .ok_or_else(|| "storage move plans require one affected torrent".to_owned())?
            .clone();
        let (source, destination) = Self::storage_move_plan_paths(plan)?;
        let (name, current_save_path) = {
            let registry = self.registry.read().await;
            let entry = registry
                .get(&info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            (entry.name.clone(), PathBuf::from(&entry.save_path))
        };
        if current_save_path != source {
            return Err(format!(
                "storage move source {} does not match torrent {info_hash} save path {}",
                source.display(),
                current_save_path.display()
            ));
        }
        Ok((info_hash, Some(name), current_save_path, destination))
    }

    fn storage_move_plan_paths(plan: &StoragePlan) -> Result<(PathBuf, PathBuf), String> {
        match plan.steps.as_slice() {
            [step] if step.action == PlannedStorageAction::Rename => Ok((
                step.source
                    .clone()
                    .ok_or_else(|| "storage move plan is missing its source path".to_owned())?,
                step.destination.clone().ok_or_else(|| {
                    "storage move plan is missing its destination path".to_owned()
                })?,
            )),
            [copy, rename, delete]
                if copy.action == PlannedStorageAction::CopyVerifyRename
                    && rename.action == PlannedStorageAction::Rename
                    && delete.action == PlannedStorageAction::SafeDelete
                    && copy.destination == rename.source
                    && delete.source == copy.source
                    && rename.destination.is_some() =>
            {
                Ok((
                    copy.source
                        .clone()
                        .ok_or_else(|| "storage move plan is missing its source path".to_owned())?,
                    rename.destination.clone().ok_or_else(|| {
                        "storage move plan is missing its destination path".to_owned()
                    })?,
                ))
            }
            _ => Err(
                "storage move plan has an unsupported or ambiguous filesystem step sequence"
                    .to_owned(),
            ),
        }
    }

    fn storage_move_context_for_plan(
        plan: &StoragePlan,
        context: &serde_json::Value,
    ) -> Result<(PathBuf, PathBuf, Option<String>), String> {
        let (plan_source, plan_destination) = Self::storage_move_plan_paths(plan)?;
        let context = context
            .as_object()
            .ok_or_else(|| "storage move job context is not an object".to_owned())?;
        let old_save_path = context
            .get("old_save_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "storage move job has no valid old_save_path context".to_owned())?;
        let save_path = context
            .get("save_path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "storage move job has no valid save_path context".to_owned())?;
        if old_save_path != plan_source {
            return Err(format!(
                "storage move context source {} does not match plan source {}",
                old_save_path.display(),
                plan_source.display()
            ));
        }
        if save_path != plan_destination {
            return Err(format!(
                "storage move context destination {} does not match plan destination {}",
                save_path.display(),
                plan_destination.display()
            ));
        }
        let name = match context.get("name") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
                    .ok_or_else(|| "storage move job has invalid name context".to_owned())?
                    .to_owned(),
            ),
        };
        Ok((old_save_path, save_path, name))
    }

    /// Return the active durable job touching one torrent. Admission checks
    /// use the already-bounded active-job set rather than scanning torrent
    /// rows or metainfo. Storage moves, deletes, and rechecks must not overlap
    /// for the same torrent because each can change the payload or its
    /// durable projection.
    async fn active_torrent_job(&self, info_hash: &str) -> CmdResult<Option<(String, String)>> {
        let info_hash = info_hash.to_owned();
        self.run_db("find_active_torrent_job", move |db| {
            rt_db::list_active_jobs(db)
                .map_err(|error| error.to_string())
                .map(|jobs| {
                    jobs.into_iter()
                        .find(|job| job.affected_torrents.iter().any(|hash| hash == &info_hash))
                        .map(|job| (job.job_id, job.kind))
                })
        })
        .await
    }

    /// Reject commands that would touch a torrent while a storage operation
    /// owns its payload. Lifecycle commands may still control an active
    /// recheck job; storage work itself may not overlap.
    async fn ensure_torrent_storage_idle(&self, info_hash: &str) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        if let Some((job_id, kind)) = self.active_torrent_job(info_hash).await? {
            if kind == JOB_KIND_STORAGE_PLAN {
                return Err(format!(
                    "torrent {info_hash} already has active storage job {job_id}"
                ));
            }
        }
        Ok(())
    }

    /// Reject a new storage/projection operation while any durable job for
    /// the torrent is active. This closes the move-vs-recheck and
    /// move-vs-delete races at the engine admission boundary.
    async fn ensure_torrent_jobs_idle(&self, info_hash: &str) -> CmdResult<()> {
        self.ensure_torrent_not_deleting(info_hash)?;
        if let Some((job_id, kind)) = self.active_torrent_job(info_hash).await? {
            let label = if kind == JOB_KIND_STORAGE_PLAN {
                "storage"
            } else if kind == JOB_KIND_RECHECK {
                "recheck"
            } else {
                kind.as_str()
            };
            return Err(format!(
                "torrent {info_hash} already has active {label} job {job_id}"
            ));
        }
        Ok(())
    }

    async fn ensure_torrents_jobs_idle(&self, info_hashes: &[String]) -> CmdResult<()> {
        for info_hash in info_hashes {
            self.ensure_torrent_jobs_idle(info_hash).await?;
        }
        Ok(())
    }

    fn ensure_torrent_not_deleting(&self, info_hash: &str) -> CmdResult<()> {
        if self.runtime.pending_torrent_deletes.contains(info_hash) {
            Err(format!(
                "torrent {info_hash} is being removed; wait for payload cleanup"
            ))
        } else {
            Ok(())
        }
    }

    async fn restore_registry_entry(
        &self,
        info_hash: &str,
        previous: TorrentEntry,
        was_dormant: bool,
    ) {
        let mut reg = self.registry.write().await;
        if let Some(mut entry) = reg.get_mut(info_hash) {
            *entry = previous;
        }
        if was_dormant {
            let _ = reg.demote(info_hash);
        }
    }

    async fn global_limits_inner(&self) -> CmdResult<EngineGlobalLimits> {
        self.run_db("get_global_limits", |db| {
            Ok(EngineGlobalLimits {
                download_limit: setting_i64_checked(db, SETTING_GLOBAL_DOWNLOAD_LIMIT)?,
                upload_limit: setting_i64_checked(db, SETTING_GLOBAL_UPLOAD_LIMIT)?,
                speed_limits_mode: setting_bool_checked(db, SETTING_GLOBAL_SPEED_LIMITS_MODE)?,
            })
        })
        .await
    }

    async fn apply_shared_global_limits_from_db(&self) -> CmdResult<()> {
        let limits = self.global_limits_inner().await?;
        self.apply_shared_global_limits(&limits);
        Ok(())
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
        self.services.network_budget.set_download_limit(download);
        self.services.network_budget.set_upload_limit(upload);
    }

    async fn update_global_limits_inner(&self, limits: EngineGlobalLimits) -> CmdResult<()> {
        let now = unix_now_i64();
        let db_limits = limits.clone();
        self.run_db("update_global_limits", move |db| {
            let tx = db.transaction().map_err(|e| e.to_string())?;
            rt_db::set_setting_in_tx(
                &tx,
                SETTING_GLOBAL_DOWNLOAD_LIMIT,
                &db_limits.download_limit.max(0).to_string(),
                now,
            )
            .map_err(|e| e.to_string())?;
            rt_db::set_setting_in_tx(
                &tx,
                SETTING_GLOBAL_UPLOAD_LIMIT,
                &db_limits.upload_limit.max(0).to_string(),
                now,
            )
            .map_err(|e| e.to_string())?;
            rt_db::set_setting_in_tx(
                &tx,
                SETTING_GLOBAL_SPEED_LIMITS_MODE,
                if db_limits.speed_limits_mode {
                    "1"
                } else {
                    "0"
                },
                now,
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        })
        .await?;
        self.apply_shared_global_limits(&limits);
        Ok(())
    }

    #[cfg(test)]
    fn network_features_inner(&self) -> CmdResult<EngineNetworkFeatures> {
        let db = self.db.lock().expect("database mutex poisoned");
        let dht_enabled =
            setting_bool_with_default_checked(&db, SETTING_NETWORK_DHT, self.config.dht.enabled)?;
        let pex_enabled = setting_bool_with_default_checked(&db, SETTING_NETWORK_PEX, true)?;
        Ok(EngineNetworkFeatures {
            dht: self
                .services
                .dht_tx
                .as_ref()
                .is_some_and(|tx| !tx.is_closed())
                && dht_enabled,
            pex: pex_enabled,
        })
    }

    async fn update_network_features_inner(
        &mut self,
        features: EngineNetworkFeatures,
    ) -> CmdResult<()> {
        {
            let now = unix_now_i64();
            self.run_db("update_network_features", move |db| {
                let tx = db.transaction().map_err(|e| e.to_string())?;
                rt_db::set_setting_in_tx(
                    &tx,
                    SETTING_NETWORK_DHT,
                    if features.dht { "1" } else { "0" },
                    now,
                )
                .map_err(|e| e.to_string())?;
                rt_db::set_setting_in_tx(
                    &tx,
                    SETTING_NETWORK_PEX,
                    if features.pex { "1" } else { "0" },
                    now,
                )
                .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())
            })
            .await?;
        }

        let dht_running = self
            .services
            .dht_tx
            .as_ref()
            .is_some_and(|tx| !tx.is_closed());
        match (features.dht, dht_running) {
            (false, true) => {
                if let Some(tx) = self.services.dht_tx.take() {
                    shutdown_dht_task(tx, Duration::from_secs(10)).await;
                }
            }
            (true, false) => {
                let tx = spawn_dht_task(&self.config);
                self.services.dht_tx = Some(tx);
                self.register_all_dht_torrents().await;
            }
            _ => {}
        }
        try_broadcast_torrent_command(
            self.runtime.torrent_chans.values(),
            || TorrentCmd::UpdatePeerExchange(features.pex),
            "update_peer_exchange",
        )
    }

    async fn set_user_agent_inner(&self, user_agent: String) -> CmdResult<()> {
        let user_agent = crate::peer_id::validate_user_agent(&user_agent)?;
        let persisted_user_agent = user_agent.clone();
        self.run_db("set_user_agent", move |db| {
            rt_db::set_setting(
                db,
                SETTING_NETWORK_USER_AGENT,
                &persisted_user_agent,
                unix_now_i64(),
            )
            .map_err(|error| error.to_string())
        })
        .await?;
        crate::peer_id::set_user_agent(user_agent)
    }

    async fn peer_exchange_enabled(&self) -> bool {
        match self
            .run_db("get_peer_exchange_setting", |db| {
                setting_bool_with_default_checked(db, SETTING_NETWORK_PEX, true)
            })
            .await
        {
            Ok(enabled) => enabled,
            Err(error) => {
                warn!(
                    component = "engine",
                    operation = "read_peer_exchange_setting",
                    result = "disabled",
                    error = %error,
                    "disabling PEX after a malformed or unavailable persisted setting"
                );
                false
            }
        }
    }

    #[cfg(test)]
    fn queue_priority_inner(&self, info_hash: &str) -> CmdResult<i32> {
        let db = self.db.lock().expect("database mutex poisoned");
        let key = queue_setting_key(info_hash);
        i32::try_from(setting_i64_checked(&db, &key)?)
            .map_err(|_| format!("queue position for {info_hash} exceeds i32"))
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
            reg.iter()
                .map(|entry| (entry.info_hash.clone(), entry.added_at))
                .collect::<Vec<_>>()
        };
        rows.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        self.run_db("update_queue_order", move |db| {
            let mut ordered = rows
                .into_iter()
                .map(|(info_hash, _added_at)| -> CmdResult<_> {
                    let pos = setting_i64_checked(db, &queue_setting_key(&info_hash))?;
                    Ok((info_hash, pos))
                })
                .collect::<CmdResult<Vec<_>>>()?;
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
                QueueMove::Top => stable_partition_selected(&mut hashes, &selected, true),
                QueueMove::Bottom => stable_partition_selected(&mut hashes, &selected, false),
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
            let tx = db.transaction().map_err(|e| e.to_string())?;
            for (idx, hash) in hashes.iter().enumerate() {
                rt_db::set_setting_in_tx(
                    &tx,
                    &queue_setting_key(hash),
                    &db_i64_usize(idx).to_string(),
                    now,
                )
                .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())
        })
        .await
    }

    async fn send_to_torrent(&self, info_hash: &str, cmd: TorrentCmd) -> CmdResult<()> {
        match self.runtime.torrent_chans.get(info_hash) {
            Some(tx) => send_torrent_command(tx, cmd).await,
            None => Err(format!("torrent {info_hash} not found")),
        }
    }

    /// Serve the last complete snapshot immediately and refresh it outside the
    /// actor. Stats are observability data; waiting on SQLite, DHT, or a slow
    /// torrent task here would let a dashboard request delay control-plane
    /// commands for the whole engine.
    fn request_engine_stats(&mut self) -> CmdResult<EngineStats> {
        if let Some(cache) = self.services.stats_cache.as_ref() {
            if cache.generated_at.elapsed() <= ENGINE_STATS_CACHE_TTL {
                return Ok(cache.stats.clone());
            }
        } else {
            let stats = self.fast_engine_stats();
            self.services.stats_cache = Some(subsystems::EngineStatsCache {
                generated_at: Instant::now(),
                stats,
                refresh_started_at: None,
            });
        }
        self.start_engine_stats_refresh();
        Ok(self
            .services
            .stats_cache
            .as_ref()
            .expect("fast stats initializes the cache")
            .stats
            .clone())
    }

    /// Build a bounded first-response snapshot without touching SQLite or
    /// querying any child actor. The detached collector replaces it shortly
    /// after with runtime counters and compatibility aggregates.
    fn fast_engine_stats(&self) -> EngineStats {
        // This is the actor's immediate response path. A concurrent registry
        // mutation must not turn an observability request into an actor-wide
        // wait; the detached collector will obtain a complete view later.
        let registry_stats = self
            .registry
            .try_read()
            .map(|registry| registry.stats())
            .unwrap_or_default();
        let mut stats = engine_stats_from_registry(registry_stats);
        let tier_counts = self.runtime.tier_controller.tier_counts();
        apply_activity_tier_stats(
            &mut stats,
            tier_counts,
            registry_stats.torrents_total,
            self.runtime.tier_controller.dormant_heap_bytes() as u64,
        );
        let storage_jobs = self.services.storage_jobs.stats();
        apply_storage_job_stats(
            &mut stats,
            storage_jobs.inflight as u64,
            storage_jobs.queue_depth as u64,
            storage_jobs.capacity as u64,
            storage_jobs.worker_count as u64,
            self.services.storage_jobs.is_healthy() as u64,
        );
        finalize_engine_stats_resources(
            &mut stats,
            self.services.resources.snapshot(),
            self.config.memory.pressure_constrained_pct,
            self.config.memory.pressure_critical_pct,
        );
        stats
    }

    fn start_engine_stats_refresh(&mut self) {
        let refresh_is_live = self.services.stats_cache.as_ref().is_some_and(|cache| {
            cache
                .refresh_started_at
                .is_some_and(|started| started.elapsed() <= ENGINE_STATS_REFRESH_STALE_AFTER)
        });
        if refresh_is_live {
            return;
        }
        let Some(cache) = self.services.stats_cache.as_mut() else {
            return;
        };
        cache.refresh_started_at = Some(Instant::now());

        let storage_jobs = self.services.storage_jobs.stats();
        let input = EngineStatsRefreshInput {
            registry: Arc::clone(&self.registry),
            db: self.db_executor(),
            task_channels: self
                .runtime
                .torrent_chans
                .iter()
                .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
                .collect(),
            dht_tx: self.services.dht_tx.clone(),
            storage_jobs_inflight: storage_jobs.inflight as u64,
            storage_jobs_queue_depth: storage_jobs.queue_depth as u64,
            storage_jobs_capacity: storage_jobs.capacity as u64,
            storage_workers: storage_jobs.worker_count as u64,
            storage_workers_healthy: self.services.storage_jobs.is_healthy() as u64,
            resources: self.services.resources.clone(),
            tier_counts: self.runtime.tier_controller.tier_counts(),
            dormant_runtime_heap_bytes: self.runtime.tier_controller.dormant_heap_bytes() as u64,
            pressure_constrained_pct: self.config.memory.pressure_constrained_pct,
            pressure_critical_pct: self.config.memory.pressure_critical_pct,
        };
        let cmd_tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let command = match timeout(
                ENGINE_STATS_REFRESH_DEADLINE,
                collect_engine_stats_background(input),
            )
            .await
            {
                Ok(Ok(stats)) => EngineCmd::StatsRefreshComplete {
                    stats: Box::new(stats),
                },
                Ok(Err(error)) => EngineCmd::StatsRefreshFailed { error },
                Err(_) => EngineCmd::StatsRefreshFailed {
                    error: format!(
                        "engine stats refresh exceeded {} ms",
                        ENGINE_STATS_REFRESH_DEADLINE.as_millis()
                    ),
                },
            };
            let _ = timeout(ENGINE_COMMAND_SEND_TIMEOUT, cmd_tx.send(command)).await;
        });
    }

    /// Full synchronous collector retained for direct engine tests and for
    /// callers that exercise the internal actor in isolation. Public command
    /// handling uses `request_engine_stats`, which never awaits this path.
    #[cfg(test)]
    async fn engine_stats(&mut self) -> CmdResult<EngineStats> {
        if let Some(cache) = self.services.stats_cache.as_ref() {
            if cache.generated_at.elapsed() <= ENGINE_STATS_CACHE_TTL {
                return Ok(cache.stats.clone());
            }
        }
        let mut stats = EngineStats::default();
        let (registry_stats, mut states) = {
            let reg = self.registry.read().await;
            let registry_stats = reg.stats();
            // Only promoted tasks need actor runtime queries below. Dormant
            // rows are represented by the registry aggregate and do not
            // force a 100k-entry traversal on every stats refresh.
            let states = self
                .runtime
                .torrent_chans
                .keys()
                .filter_map(|info_hash| {
                    reg.get(info_hash)
                        .map(|entry| (info_hash.clone(), entry.state))
                })
                .collect::<HashMap<_, _>>();
            (registry_stats, states)
        };
        stats.torrents_total = registry_stats.torrents_total;
        stats.torrents_seeding = registry_stats.torrents_seeding;
        stats.torrents_downloading = registry_stats.torrents_downloading;
        stats.torrents_paused = registry_stats
            .torrents_stopped
            .saturating_add(registry_stats.torrents_paused);
        stats.torrents_checking = registry_stats.torrents_checking;
        stats.torrents_queued = registry_stats.torrents_queued;
        stats.torrents_error = registry_stats.torrents_error;
        stats.torrents_metadata_pending = registry_stats.torrents_metadata_pending;
        stats.bytes_uploaded = registry_stats.bytes_uploaded;
        stats.bytes_downloaded = registry_stats.bytes_downloaded;
        stats.bytes_left = registry_stats.bytes_left;
        let now = Instant::now();
        // These aggregate queries are compatibility/read-model work, not
        // actor ordering. Never make the engine actor wait on the database
        // mutex or execute SQLite while it is responsible for command
        // dispatch. A busy writer causes a bounded stats miss instead of
        // stalling every torrent command behind the stats cache refresh.
        let db = Arc::clone(&self.db);
        let tracker_counts = match tokio::task::spawn_blocking(move || {
            let db = db
                .try_lock()
                .map_err(|_| "database busy while collecting engine stats".to_owned())?;
            let jobs = rt_db::count_active_jobs(&db).map_err(|e| e.to_string())?;
            let trackers = rt_db::torrent_tracker_status_counts(&db).map_err(|e| e.to_string())?;
            Ok::<_, String>((jobs, trackers))
        })
        .await
        {
            Ok(Ok((jobs, trackers))) => {
                stats.jobs_active = jobs;
                trackers
            }
            Ok(Err(error)) => {
                warn!(
                    component = "engine",
                    operation = "collect_database_stats",
                    result = "unavailable",
                    error = %error,
                    "engine database aggregate stats unavailable"
                );
                return Err(error);
            }
            Err(error) => {
                warn!(
                    component = "engine",
                    operation = "collect_database_stats",
                    result = "worker_failed",
                    error = %error,
                    "engine database aggregate stats worker failed"
                );
                return Err(format!("engine database stats worker failed: {error}"));
            }
        };
        let storage_jobs = self.services.storage_jobs.stats();
        stats.storage_jobs_inflight = storage_jobs.inflight as u64;
        stats.storage_jobs_queue_depth = storage_jobs.queue_depth as u64;
        stats.storage_jobs_capacity = storage_jobs.capacity as u64;
        stats.storage_workers = storage_jobs.worker_count as u64;
        stats.storage_workers_healthy = self.services.storage_jobs.is_healthy() as u64;
        stats.trackers_total = tracker_counts.total;
        stats.trackers_working = tracker_counts.working;
        stats.trackers_warning = tracker_counts.warning;
        stats.trackers_error = tracker_counts.error;
        if let Some(dht_tx) = &self.services.dht_tx {
            let (reply, rx) = tokio::sync::oneshot::channel();
            if dht_tx.try_send(DhtCommand::GetStats { reply }).is_ok() {
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
            .runtime
            .torrent_chans
            .iter()
            .map(|(info_hash, tx)| (info_hash.clone(), tx.clone()))
            .collect::<Vec<_>>();
        let runtime_results = timeout(
            ENGINE_STATS_TASK_QUERY_DEADLINE,
            stream::iter(task_channels.into_iter().map(|(info_hash, tx)| async move {
                let (reply, rx) = tokio::sync::oneshot::channel();
                let send_result = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    tx.send(TorrentCmd::GetRuntimeStats { reply }),
                )
                .await;
                if !matches!(send_result, Ok(Ok(()))) {
                    return (info_hash, None);
                }
                match timeout(ENGINE_STATS_TASK_QUERY_DEADLINE, rx).await {
                    Ok(Ok(runtime)) => (info_hash, Some(runtime)),
                    Ok(Err(_)) => (info_hash, None),
                    Err(_) => {
                        warn!(
                            component = "engine",
                            operation = "collect_runtime_stats",
                            target = "torrent",
                            torrent = %info_hash,
                            duration_ms = ENGINE_STATS_TASK_QUERY_DEADLINE.as_millis(),
                            result = "timeout",
                            "timed out collecting torrent runtime stats"
                        );
                        (info_hash, None)
                    }
                }
            }))
            .buffer_unordered(64)
            .collect::<Vec<_>>(),
        )
        .await
        .unwrap_or_default();

        for (info_hash, runtime) in runtime_results {
            let Some(state) = states.remove(&info_hash) else {
                continue;
            };
            if let Some(runtime) = runtime {
                self.runtime.tier_controller.apply_input(
                    info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: runtime.connected_peers as usize,
                        outstanding_requests: runtime.outstanding_requests as usize,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                        now,
                    },
                );
                stats.add_torrent_runtime(info_hash, runtime);
            } else {
                if self.runtime.tier_controller.tier(&info_hash).is_none() {
                    self.runtime.tier_controller.apply_input(
                        info_hash.clone(),
                        TierInput {
                            state,
                            connected_peers: 0,
                            outstanding_requests: 0,
                            inbound_peer: false,
                            tracker_due: false,
                            last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                            now,
                        },
                    );
                }
            }
        }
        for (info_hash, state) in states {
            if self.runtime.tier_controller.tier(&info_hash).is_none() {
                self.runtime.tier_controller.apply_input(
                    info_hash.clone(),
                    TierInput {
                        state,
                        connected_peers: 0,
                        outstanding_requests: 0,
                        inbound_peer: false,
                        tracker_due: false,
                        last_active: self.runtime.tier_last_active.get(&info_hash).copied(),
                        now,
                    },
                );
            }
        }
        // The channel map is the authoritative set of promoted torrent
        // actors. Runtime-stat replies are best-effort and must not make this
        // gauge lag behind a demotion or disappear when one actor is slow.
        stats.torrent_tasks_active = self.runtime.torrent_chans.len() as u64;
        let [dormant, warm, hot] = self.runtime.tier_controller.tier_counts();
        let tracked = dormant.saturating_add(warm).saturating_add(hot);
        stats.torrents_activity_dormant =
            dormant as u64 + registry_stats.torrents_total.saturating_sub(tracked as u64);
        stats.torrents_activity_warm = warm as u64;
        stats.torrents_activity_hot = hot as u64;
        stats.dormant_runtime_heap_bytes = self.runtime.tier_controller.dormant_heap_bytes() as u64;
        let mut resources = self.services.resources.snapshot();
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
        self.services.stats_cache = Some(subsystems::EngineStatsCache {
            generated_at: Instant::now(),
            stats: stats.clone(),
            refresh_started_at: None,
        });
        Ok(stats)
    }

    async fn engine_subsystem_health(&self) -> CmdResult<EngineSubsystemHealth> {
        #[cfg(not(test))]
        let db_worker_healthy = self.db_worker.is_healthy();
        #[cfg(test)]
        let db_worker_healthy = true;
        let dht_enabled = self.services.dht_tx.is_some();
        let dht_healthy = if let Some(dht_tx) = &self.services.dht_tx {
            if dht_tx.is_closed() {
                false
            } else {
                let (reply, response) = oneshot::channel();
                if dht_tx.try_send(DhtCommand::GetStats { reply }).is_err() {
                    false
                } else {
                    matches!(
                        timeout(Duration::from_millis(250), response).await,
                        Ok(Ok(_))
                    )
                }
            }
        } else {
            true
        };
        Ok(EngineSubsystemHealth {
            db_worker_healthy,
            storage_workers_healthy: self.services.storage_jobs.is_healthy(),
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
        let taskless_v2 = !self.runtime.torrent_chans.contains_key(info_hash)
            && self
                .metadata_placeholder_row_checked(info_hash)
                .await?
                .is_none()
            && self.is_pure_v2_torrent(info_hash);
        let info_hash_for_db = info_hash.to_owned();
        let (is_private, trackers, active_jobs) = self
            .run_db("diagnose_torrent", move |db| {
                let row = rt_db::get(db, &info_hash_for_db).map_err(|e| e.to_string())?;
                let trackers = rt_db::list_torrent_trackers(db, &info_hash_for_db)
                    .map_err(|error| error.to_string())?;
                let active_jobs = rt_db::list_active_jobs(db)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|job| {
                        job.affected_torrents
                            .iter()
                            .any(|hash| hash == &info_hash_for_db)
                    })
                    .count();
                Ok::<_, String>((row.is_private, trackers, active_jobs))
            })
            .await?;
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

    async fn control_recheck_job(&mut self, job_id: &str, target_state: &str) -> CmdResult<()> {
        let job_id_for_db = job_id.to_owned();
        let job = self
            .run_db("load_job_for_control", move |db| {
                rt_db::get_job(db, &job_id_for_db).map_err(|error| error.to_string())
            })
            .await?;
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
                    JOB_STATE_CANCELLED
                        | JOB_STATE_COMPLETED
                        | JOB_STATE_FAILED
                        | STORAGE_JOB_STATE_COMMIT_PENDING
                )
            {
                return Err(format!("job {job_id} is already terminal"));
            }
            self.services.storage_jobs.control(job_id, action)?;
            self.update_job_state_async(
                job_id,
                target_state,
                None,
                Some("storage plan job control updated"),
            )
            .await?;
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
        self.ensure_torrent_not_deleting(&info_hash)?;
        if target_state == JOB_STATE_PAUSED
            && self
                .cancel_pending_recheck_job(
                    &info_hash,
                    job_id,
                    JOB_STATE_PAUSED,
                    "recheck promotion paused before task creation",
                )
                .await
        {
            return Ok(());
        }
        if target_state == JOB_STATE_CANCELLED
            && self
                .cancel_pending_recheck_job(
                    &info_hash,
                    job_id,
                    JOB_STATE_CANCELLED,
                    "recheck promotion cancelled before task creation",
                )
                .await
        {
            return Ok(());
        }
        if target_state == JOB_STATE_RUNNING && self.has_pending_recheck_job(&info_hash, job_id) {
            // The original recheck action is already retained by the
            // coalesced promotion worker. Do not send a second command or
            // mark the job running before the task exists; the eventual
            // promotion completion performs the dispatch transition.
            self.update_job_state_async(
                job_id,
                JOB_STATE_QUEUED,
                None,
                Some("recheck promotion remains queued"),
            )
            .await?;
            return Ok(());
        }
        let taskless_v2 = !self.runtime.torrent_chans.contains_key(&info_hash)
            && self
                .metadata_placeholder_row_checked(&info_hash)
                .await?
                .is_none()
            && self.is_pure_v2_torrent(&info_hash);
        match target_state {
            JOB_STATE_PAUSED => {
                if taskless_v2 {
                    self.set_registry_state(&info_hash, TorrentState::Paused, None)
                        .await?;
                } else {
                    self.send_to_torrent(&info_hash, TorrentCmd::Pause).await?;
                }
                self.update_job_state_async(
                    job_id,
                    JOB_STATE_PAUSED,
                    None,
                    Some("recheck job paused"),
                )
                .await?;
            }
            JOB_STATE_RUNNING => {
                if taskless_v2 {
                    self.start_pure_v2_recheck(&info_hash, Some(job_id.to_owned()))
                        .await?;
                } else if self.runtime.torrent_chans.contains_key(&info_hash) {
                    self.send_to_torrent(
                        &info_hash,
                        TorrentCmd::Recheck {
                            job_id: Some(job_id.to_owned()),
                        },
                    )
                    .await?;
                    self.update_job_state_async(
                        job_id,
                        JOB_STATE_RUNNING,
                        None,
                        Some("recheck job resumed"),
                    )
                    .await?;
                } else {
                    let (reply, _reply_rx) = oneshot::channel();
                    match self.begin_torrent_task_promotion(
                        &info_hash,
                        TorrentPromotionAction::Recheck {
                            job_id: Some(job_id.to_owned()),
                            reply,
                        },
                    ) {
                        TorrentPromotionBegin::Ready(action) => {
                            self.execute_torrent_promotion_action(&info_hash, *action, false)
                                .await;
                        }
                        TorrentPromotionBegin::Pending => {
                            self.update_job_state_async(
                                job_id,
                                JOB_STATE_QUEUED,
                                None,
                                Some("recheck promotion queued"),
                            )
                            .await?;
                        }
                    }
                }
            }
            JOB_STATE_CANCELLED => {
                if !taskless_v2 && self.runtime.torrent_chans.contains_key(&info_hash) {
                    self.send_to_torrent(
                        &info_hash,
                        TorrentCmd::CancelJob {
                            job_id: job_id.to_owned(),
                        },
                    )
                    .await?;
                }
                self.update_job_state_async(
                    job_id,
                    JOB_STATE_CANCELLED,
                    None,
                    Some("recheck job cancelled"),
                )
                .await?;
            }
            _ => return Err(format!("unsupported job state {target_state}")),
        }
        Ok(())
    }

    fn has_pending_recheck_job(&self, info_hash: &str, job_id: &str) -> bool {
        self.runtime
            .pending_torrent_promotions
            .get(info_hash)
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    matches!(
                        action,
                        TorrentPromotionAction::Recheck {
                            job_id: Some(candidate),
                            ..
                        } if candidate == job_id
                    )
                })
            })
    }

    async fn cancel_pending_recheck_job(
        &mut self,
        info_hash: &str,
        job_id: &str,
        target_state: &str,
        reason: &str,
    ) -> bool {
        let Some(actions) = self.runtime.pending_torrent_promotions.get_mut(info_hash) else {
            return false;
        };
        let Some(index) = actions.iter().position(|action| {
            matches!(
                action,
                TorrentPromotionAction::Recheck {
                    job_id: Some(candidate),
                    ..
                } if candidate == job_id
            )
        }) else {
            return false;
        };
        let action = actions.remove(index);
        if actions.is_empty() {
            self.runtime.pending_torrent_promotions.remove(info_hash);
        }
        if let TorrentPromotionAction::Recheck { reply, .. } = action {
            let _ = reply.send(Err(reason.to_owned()));
        }
        self.update_job_state_best_effort(job_id, target_state, None, Some(reason))
            .await;
        true
    }

    async fn ensure_metadata_task(&mut self, info_hash_hex: &str) -> CmdResult<()> {
        if self.runtime.torrent_chans.contains_key(info_hash_hex) {
            return Ok(());
        }
        let info_hash_for_db = info_hash_hex.to_owned();
        let row = self
            .run_db("load_metadata_torrent", move |db| {
                rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())
            })
            .await?;
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
            state_from_str(&row.state),
        );
        Ok(())
    }

    /// Begin reconstructing a dormant torrent without occupying the engine
    /// actor on metainfo I/O or parsing. Requests for the same torrent share
    /// one worker and retain their continuations until that worker reports
    /// back through the actor command queue.
    fn begin_torrent_task_promotion(
        &mut self,
        info_hash_hex: &str,
        action: TorrentPromotionAction,
    ) -> TorrentPromotionBegin {
        if self.runtime.torrent_chans.contains_key(info_hash_hex) {
            return TorrentPromotionBegin::Ready(Box::new(action));
        }

        let should_start_worker = match self
            .runtime
            .pending_torrent_promotions
            .entry(info_hash_hex.to_owned())
        {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(vec![action]);
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().push(action);
                false
            }
        };
        if should_start_worker {
            let config = Arc::clone(&self.config);
            let db = self.db_executor();
            let cmd_tx = self.cmd_tx.clone();
            let info_hash = info_hash_hex.to_owned();
            tokio::spawn(async move {
                let worker_result = tokio::task::spawn_blocking({
                    let config = Arc::clone(&config);
                    let db = db.clone();
                    let info_hash = info_hash.clone();
                    move || prepare_torrent_task_from_storage(&config, &db, &info_hash)
                })
                .await;
                let prepared = match worker_result {
                    Ok(result) => result,
                    Err(error) => Err(format!("torrent promotion worker failed: {error}")),
                };
                let _ = timeout(
                    ENGINE_COMMAND_SEND_TIMEOUT,
                    cmd_tx.send(EngineCmd::PreparedTorrentTask {
                        info_hash,
                        prepared,
                    }),
                )
                .await;
            });
        }
        TorrentPromotionBegin::Pending
    }

    async fn finish_prepared_torrent_task(
        &mut self,
        info_hash: String,
        prepared: CmdResult<PreparedTorrentTaskData>,
    ) {
        let actions = self
            .runtime
            .pending_torrent_promotions
            .remove(&info_hash)
            .unwrap_or_default();
        if actions.is_empty() {
            return;
        }
        if let Err(error) = self.ensure_torrent_storage_idle(&info_hash).await {
            self.fail_torrent_promotion_actions(&info_hash, actions, error)
                .await;
            return;
        }

        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_torrent_promotion_actions(&info_hash, actions, error)
                    .await;
                return;
            }
        };
        let current_save_path = self
            .registry
            .read()
            .await
            .get(&info_hash)
            .map(|entry| (PathBuf::from(&entry.save_path), entry.state));
        let Some((current_save_path, initial_state)) = current_save_path else {
            self.fail_torrent_promotion_actions(
                &info_hash,
                actions,
                format!("torrent {info_hash} not found"),
            )
            .await;
            return;
        };
        if current_save_path != prepared.save_path {
            self.fail_torrent_promotion_actions(
                &info_hash,
                actions,
                "torrent save path changed while promotion was in progress; retry the request"
                    .to_owned(),
            )
            .await;
            return;
        }

        self.runtime
            .tier_controller
            .cancel_tracker_check(&info_hash);
        self.runtime
            .tier_controller
            .clear_dormant_snapshot(&info_hash);
        let _tx = self
            .spawn_torrent_task(
                info_hash.clone(),
                prepared.meta,
                prepared.save_path,
                true,
                initial_state,
            )
            .await;
        if !prepared.is_private {
            self.register_dht_torrent(prepared.info_hash, &info_hash)
                .await;
        }
        for action in actions {
            self.execute_torrent_promotion_action(&info_hash, action, true)
                .await;
        }
    }

    async fn execute_torrent_promotion_action(
        &mut self,
        info_hash: &str,
        action: TorrentPromotionAction,
        resume_before_action: bool,
    ) {
        match action {
            TorrentPromotionAction::Resume { reply } => {
                let result = self.send_to_torrent(info_hash, TorrentCmd::Resume).await;
                if result.is_ok() {
                    self.append_session_event(
                        Some(info_hash),
                        EVENT_TORRENT_RESUMED,
                        Some("torrent resumed"),
                        serde_json::json!({
                            "v2_only": false,
                            "skipped": false,
                        }),
                    );
                }
                let _ = reply.send(result);
            }
            TorrentPromotionAction::Recheck { job_id, reply } => {
                let result = self
                    .send_to_torrent(
                        info_hash,
                        TorrentCmd::Recheck {
                            job_id: job_id.clone(),
                        },
                    )
                    .await;
                self.record_recheck_dispatch(info_hash, job_id.as_deref(), &result)
                    .await;
                let _ = reply.send(result);
            }
            TorrentPromotionAction::Reannounce { reply } => {
                let result = if resume_before_action {
                    match self.send_to_torrent(info_hash, TorrentCmd::Resume).await {
                        Ok(()) => {
                            self.send_to_torrent(info_hash, TorrentCmd::Reannounce)
                                .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    self.send_to_torrent(info_hash, TorrentCmd::Reannounce)
                        .await
                };
                if result.is_ok() {
                    self.append_session_event(
                        Some(info_hash),
                        EVENT_REANNOUNCE_REQUESTED,
                        Some("torrent reannounce requested"),
                        serde_json::json!({
                            "v2_only": false,
                            "skipped": false,
                        }),
                    );
                }
                let _ = reply.send(result);
            }
            TorrentPromotionAction::AddPeers { peers, reply } => {
                let result = if resume_before_action {
                    match self.send_to_torrent(info_hash, TorrentCmd::Resume).await {
                        Ok(()) => {
                            self.send_to_torrent(info_hash, TorrentCmd::PriorityPeers(peers))
                                .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    self.send_to_torrent(info_hash, TorrentCmd::PriorityPeers(peers))
                        .await
                };
                let _ = reply.send(result);
            }
            TorrentPromotionAction::IncomingPeer { command } => {
                let result = if resume_before_action {
                    match self.send_to_torrent(info_hash, TorrentCmd::Resume).await {
                        Ok(()) => self.send_to_torrent(info_hash, *command).await,
                        Err(error) => Err(error),
                    }
                } else {
                    self.send_to_torrent(info_hash, *command).await
                };
                if let Err(error) = result {
                    warn!(
                        component = "peer_listener",
                        operation = "route_promoted_peer",
                        torrent = %info_hash,
                        result = "rejected",
                        error = %error,
                        "promoted torrent task stopped before peer delivery"
                    );
                } else {
                    let now = Instant::now();
                    self.runtime
                        .tier_last_active
                        .insert(info_hash.to_owned(), now);
                }
            }
            TorrentPromotionAction::TrackerReannounce => {
                let result = if resume_before_action {
                    match self.send_to_torrent(info_hash, TorrentCmd::Resume).await {
                        Ok(()) => {
                            self.send_to_torrent(info_hash, TorrentCmd::Reannounce)
                                .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    self.send_to_torrent(info_hash, TorrentCmd::Reannounce)
                        .await
                };
                if result.is_err() {
                    self.runtime.tier_controller.schedule_tracker_check(
                        info_hash.to_owned(),
                        Instant::now() + Duration::from_secs(30),
                    );
                    warn!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "error",
                        error = ?result.err(),
                        "promoted tracker-due torrent failed before reannounce"
                    );
                } else {
                    self.runtime
                        .tier_last_active
                        .insert(info_hash.to_owned(), Instant::now());
                    info!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "ok",
                        "promoted dormant torrent for persisted tracker deadline"
                    );
                }
            }
        }
    }

    async fn fail_torrent_promotion_actions(
        &mut self,
        info_hash: &str,
        actions: Vec<TorrentPromotionAction>,
        error: String,
    ) {
        for action in actions {
            match action {
                TorrentPromotionAction::Resume { reply } => {
                    let _ = reply.send(Err(error.clone()));
                }
                TorrentPromotionAction::Recheck { job_id, reply } => {
                    if let Some(job_id) = job_id {
                        self.update_job_state_best_effort(
                            &job_id,
                            JOB_STATE_FAILED,
                            Some(error.clone()),
                            Some("recheck promotion failed"),
                        )
                        .await;
                    }
                    let _ = reply.send(Err(error.clone()));
                }
                TorrentPromotionAction::Reannounce { reply } => {
                    let _ = reply.send(Err(error.clone()));
                }
                TorrentPromotionAction::AddPeers { reply, .. } => {
                    let _ = reply.send(Err(error.clone()));
                }
                TorrentPromotionAction::IncomingPeer { .. } => {
                    warn!(
                        component = "peer_listener",
                        operation = "promote_incoming_peer",
                        torrent = %info_hash,
                        result = "rejected",
                        error = %error,
                        "failed to promote dormant torrent for inbound peer"
                    );
                }
                TorrentPromotionAction::TrackerReannounce => {
                    self.runtime.tier_controller.schedule_tracker_check(
                        info_hash.to_owned(),
                        Instant::now() + Duration::from_secs(30),
                    );
                    warn!(
                        component = "tiering",
                        operation = "promote_tracker_due",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "failed to promote dormant torrent for tracker deadline"
                    );
                }
            }
        }
    }

    async fn fail_all_pending_torrent_promotions(&mut self) {
        let pending = std::mem::take(&mut self.runtime.pending_torrent_promotions);
        for (info_hash, actions) in pending {
            self.fail_torrent_promotion_actions(
                &info_hash,
                actions,
                "engine shutting down before torrent promotion completed".to_owned(),
            )
            .await;
        }
    }

    async fn cancel_pending_torrent_promotion(&mut self, info_hash: &str, reason: &str) {
        let Some(actions) = self.runtime.pending_torrent_promotions.remove(info_hash) else {
            return;
        };
        for action in actions {
            match action {
                TorrentPromotionAction::Resume { reply }
                | TorrentPromotionAction::Reannounce { reply }
                | TorrentPromotionAction::AddPeers { reply, .. } => {
                    let _ = reply.send(Err(reason.to_owned()));
                }
                TorrentPromotionAction::Recheck { job_id, reply } => {
                    if let Some(job_id) = job_id {
                        self.update_job_state_best_effort(
                            &job_id,
                            JOB_STATE_PAUSED,
                            Some(reason.to_owned()),
                            Some("recheck promotion cancelled"),
                        )
                        .await;
                    }
                    let _ = reply.send(Err(reason.to_owned()));
                }
                TorrentPromotionAction::IncomingPeer { .. } => warn!(
                    component = "peer_listener",
                    operation = "cancel_promoted_peer",
                    torrent = %info_hash,
                    result = "cancelled",
                    reason,
                    "dropped inbound peer while cancelling dormant promotion"
                ),
                TorrentPromotionAction::TrackerReannounce => {}
            }
        }
    }

    async fn record_recheck_dispatch(
        &self,
        info_hash: &str,
        job_id: Option<&str>,
        result: &CmdResult<()>,
    ) {
        if result.is_ok() {
            if let Some(job_id) = job_id {
                self.update_job_state_best_effort(
                    job_id,
                    JOB_STATE_RUNNING,
                    None,
                    Some("recheck dispatched to torrent task"),
                )
                .await;
            }
            self.append_session_event(
                Some(info_hash),
                EVENT_RECHECK_REQUESTED,
                Some("torrent recheck requested"),
                serde_json::json!({ "job_id": job_id }),
            );
        } else if let Some(job_id) = job_id {
            self.update_job_state_best_effort(
                job_id,
                JOB_STATE_FAILED,
                result.as_ref().err().cloned(),
                Some("recheck dispatch failed"),
            )
            .await;
        }
    }

    /// Promote a persisted, taskless torrent into the hot runtime tier. A
    /// dormant seed keeps only its registry/SQLite/blob state; promotion
    /// reconstructs the task from the authoritative metainfo blob and then
    /// lets the normal command path resume/recheck it.
    #[cfg(test)]
    async fn ensure_torrent_task(&mut self, info_hash_hex: &str) -> CmdResult<()> {
        if self.runtime.torrent_chans.contains_key(info_hash_hex) {
            return Ok(());
        }
        if self
            .metadata_placeholder_row_checked(info_hash_hex)
            .await?
            .is_some()
        {
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
        self.authorize_storage_path_async(Path::new(&row.save_path))
            .await?;
        let tier_key = info_hash_hex.to_owned();
        self.runtime.tier_controller.cancel_tracker_check(&tier_key);
        self.runtime
            .tier_controller
            .clear_dormant_snapshot(&tier_key);
        let initial_state = state_from_str(&row.state);
        let _tx = self
            .spawn_torrent_task(
                tier_key,
                v1,
                PathBuf::from(row.save_path),
                true,
                initial_state,
            )
            .await;
        if !is_private {
            self.register_dht_torrent(info_hash, info_hash_hex).await;
        }
        Ok(())
    }

    async fn register_dht_torrent(&self, info_hash: [u8; 20], info_hash_hex: &str) {
        let Some(dht_tx) = &self.services.dht_tx else {
            return;
        };
        let Some(cmd_tx) = self.runtime.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        if let Err(error) =
            dht_tx.try_send(DhtCommand::AddTorrent(DhtTorrent { info_hash, cmd_tx }))
        {
            let reason = match &error {
                mpsc::error::TrySendError::Full(_) => "queue_full",
                mpsc::error::TrySendError::Closed(_) => "channel_closed",
            };
            warn!(
                component = "dht",
                operation = "register_torrent",
                torrent = %info_hash_hex,
                result = "not_delivered",
                reason,
                error = %error,
                "DHT registration command was not delivered"
            );
        }
    }

    /// Register a torrent with DHT without making the engine actor read or
    /// parse its metainfo blob. Dormant promotion already supplies the v1
    /// identity directly; this path is for resumed metadata-pending entries,
    /// dynamic DHT enablement, and older rows that need blob inspection.
    fn register_dht_torrent_from_storage_or_hash(&self, info_hash_hex: &str) {
        let Some(dht_tx) = self.services.dht_tx.clone() else {
            return;
        };
        let Some(cmd_tx) = self.runtime.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        let config = Arc::clone(&self.config);
        let info_hash = info_hash_hex.to_owned();
        let worker_info_hash = info_hash.clone();
        tokio::spawn(async move {
            let parsed =
                tokio::task::spawn_blocking(move || -> Result<Option<[u8; 20]>, String> {
                    let blob_path = torrent_blob_path(&config, &worker_info_hash);
                    if rt_storage::metadata_no_follow(&blob_path).is_ok() {
                        let raw =
                            rt_storage::read_file_no_follow_limited(&blob_path, MAX_TORRENT_BYTES)
                                .map_err(|error| error.to_string())?;
                        let meta = parse_torrent(&raw).map_err(|error| error.to_string())?;
                        return Ok(match meta {
                            TorrentMeta::V1(meta) if !meta.private => Some(meta.info_hash),
                            TorrentMeta::Hybrid(meta, _) if !meta.private => Some(meta.info_hash),
                            _ => None,
                        });
                    }
                    Ok(parse_info_hash_hex(&worker_info_hash).ok())
                })
                .await;
            match parsed {
                Ok(Ok(Some(info_hash))) => {
                    if let Err(error) =
                        dht_tx.try_send(DhtCommand::AddTorrent(DhtTorrent { info_hash, cmd_tx }))
                    {
                        let reason = match &error {
                            mpsc::error::TrySendError::Full(_) => "queue_full",
                            mpsc::error::TrySendError::Closed(_) => "channel_closed",
                        };
                        warn!(
                            component = "dht",
                            operation = "register_torrent",
                            torrent = %hex::encode(info_hash),
                            result = "not_delivered",
                            reason,
                            error = %error,
                            "DHT registration command was not delivered"
                        );
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => warn!(
                    component = "dht",
                    operation = "register_torrent",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to load torrent metadata for DHT registration"
                ),
                Err(error) => warn!(
                    component = "dht",
                    operation = "register_torrent",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "DHT registration worker failed"
                ),
            }
        });
    }

    async fn register_all_dht_torrents(&self) {
        let hashes = {
            self.runtime
                .torrent_chans
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        for hash in hashes {
            self.register_dht_torrent_from_storage_or_hash(&hash);
        }
    }

    fn is_metadata_placeholder_row(&self, row: &TorrentRow) -> bool {
        is_metadata_placeholder_row_for(&self.config, row)
    }

    async fn metadata_placeholder_row_checked(
        &self,
        info_hash: &str,
    ) -> CmdResult<Option<TorrentRow>> {
        let info_hash_for_db = info_hash.to_owned();
        let row = self
            .run_db(
                "load_metadata_placeholder_projection",
                move |db| match rt_db::get(db, &info_hash_for_db) {
                    Ok(row) => Ok(Some(row)),
                    Err(rt_db::DbError::NotFound(_)) => Ok(None),
                    Err(error) => Err(format!(
                        "failed to load durable torrent metadata for {info_hash_for_db}: {error}"
                    )),
                },
            )
            .await?;
        Ok(row.filter(|row| self.is_metadata_placeholder_row(row)))
    }

    #[cfg(test)]
    fn metadata_placeholder_row_checked_sync(
        &self,
        info_hash: &str,
    ) -> CmdResult<Option<TorrentRow>> {
        let db = self.db.lock().expect("database mutex poisoned");
        let row = match rt_db::get(&db, info_hash) {
            Ok(row) => row,
            Err(rt_db::DbError::NotFound(_)) => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "failed to load durable torrent metadata for {info_hash}: {error}"
                ));
            }
        };
        Ok(self.is_metadata_placeholder_row(&row).then_some(row))
    }

    async fn update_metadata_placeholder_state_with_event(
        &self,
        info_hash: &str,
        state: TorrentState,
        event: Option<rt_db::SessionEventRow>,
    ) -> CmdResult<()> {
        let Some(mut row) = self.metadata_placeholder_row_checked(info_hash).await? else {
            return Err(format!("torrent {info_hash} is not metadata pending"));
        };
        row.state = state.as_str().to_owned();
        let (previous, was_dormant) = {
            let mut registry = self.registry.write().await;
            let was_dormant = registry.is_dormant(info_hash);
            let mut entry = registry
                .get_mut(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            let previous = entry.clone();
            entry.transition(state).map_err(|error| error.to_string())?;
            (previous, was_dormant)
        };
        let retention = self.config.logging.event_retention;
        let persistence = self
            .run_db("persist_metadata_placeholder_state", move |db| {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::upsert_in_tx(&tx, &row).map_err(|error| error.to_string())?;
                if let Some(event) = event.as_ref() {
                    rt_db::append_session_event_in_tx(&tx, event)
                        .map_err(|error| error.to_string())?;
                    rt_db::prune_session_events_in_tx(&tx, retention)
                        .map_err(|error| error.to_string())?;
                }
                tx.commit().map_err(|error| error.to_string())
            })
            .await;
        if let Err(error) = persistence {
            self.restore_registry_entry(info_hash, previous, was_dormant)
                .await;
            return Err(error);
        }
        Ok(())
    }

    async fn unregister_dht_torrent(&self, info_hash_hex: &str) {
        let Some(dht_tx) = &self.services.dht_tx else {
            return;
        };
        let Ok(info_hash) = parse_info_hash_hex(info_hash_hex) else {
            return;
        };
        if let Err(error) = dht_tx.try_send(DhtCommand::RemoveTorrent(info_hash)) {
            let reason = match &error {
                mpsc::error::TrySendError::Full(_) => "queue_full",
                mpsc::error::TrySendError::Closed(_) => "channel_closed",
            };
            warn!(
                component = "dht",
                operation = "unregister_torrent",
                torrent = %info_hash_hex,
                result = "not_delivered",
                reason,
                error = %error,
                "DHT unregistration command was not delivered"
            );
        }
    }

    fn session_event_row(
        &self,
        info_hash: Option<&str>,
        kind: &str,
        message: Option<&str>,
        payload: serde_json::Value,
    ) -> rt_db::SessionEventRow {
        rt_db::SessionEventRow {
            event_id: None,
            occurred_at: unix_now_i64(),
            info_hash: info_hash.map(str::to_owned),
            kind: kind.to_owned(),
            message: message.map(str::to_owned),
            payload: payload.to_string(),
        }
    }

    fn append_session_event(
        &self,
        info_hash: Option<&str>,
        kind: &str,
        message: Option<&str>,
        payload: serde_json::Value,
    ) {
        let event = self.session_event_row(info_hash, kind, message, payload);
        #[cfg(not(test))]
        {
            let Some(writer) = self.session_event_writer.as_ref() else {
                warn!(
                    component = "db",
                    operation = "append_session_event",
                    kind,
                    result = "writer_unavailable",
                    "session event writer is unavailable"
                );
                return;
            };
            if let Err(error) = writer.tx.try_send(event) {
                let result = match error {
                    mpsc::error::TrySendError::Full(_) => "queue_full",
                    mpsc::error::TrySendError::Closed(_) => "writer_closed",
                };
                warn!(
                    component = "db",
                    operation = "append_session_event",
                    kind,
                    result,
                    "dropping session event because the bounded writer cannot accept it"
                );
            }
        }
        #[cfg(test)]
        let _ = kind;
        #[cfg(test)]
        let db = self.db.lock().expect("database mutex poisoned");
        #[cfg(test)]
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

    #[cfg(test)]
    fn create_recheck_job(&self, info_hash: &str) -> Option<String> {
        let now = unix_now_i64();
        // Piece count is part of the durable torrent projection. Reading it
        // from SQLite avoids parsing the persisted metainfo blob on the
        // engine actor merely to initialize a recheck job's progress total.
        let total = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get(&db, info_hash) {
                Ok(row) => row.piece_count.max(0),
                Err(error) => {
                    warn!(
                        component = "db",
                        operation = "create_recheck_job",
                        torrent = %info_hash,
                        result = "error",
                        error = %error,
                        "cannot create recheck job without durable torrent metadata"
                    );
                    return None;
                }
            }
        };
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
        if let Err(e) = self.persist_job_with_events(&job, &[event]) {
            warn!(
                component = "db",
                operation = "persist_recheck_job_and_event",
                torrent = %info_hash,
                result = "error",
                error = %e,
                "failed to persist recheck job and event atomically"
            );
            return None;
        }
        Some(job_id)
    }

    async fn create_recheck_job_async(&self, info_hash: &str) -> CmdResult<String> {
        let info_hash_for_db = info_hash.to_owned();
        let total = self
            .run_db("load_recheck_piece_count", move |db| {
                rt_db::get(db, &info_hash_for_db)
                    .map(|row| row.piece_count.max(0))
                    .map_err(|error| {
                        format!(
                            "cannot create recheck job without durable torrent metadata for {info_hash_for_db}: {error}"
                        )
                    })
            })
            .await?;
        let now = unix_now_i64();
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
        self.persist_job_with_events_async(&job, &[event]).await?;
        Ok(job_id)
    }

    #[cfg(test)]
    fn list_storage_roots_inner(&self) -> CmdResult<Vec<EngineStorageRoot>> {
        self.list_storage_root_rows_inner()
            .map(|rows| rows.into_iter().map(engine_storage_root).collect())
    }

    #[cfg(test)]
    fn list_storage_root_rows_inner(&self) -> CmdResult<Vec<rt_db::StorageRootRow>> {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_storage_roots(&db).map_err(|e| e.to_string())
    }

    #[cfg(test)]
    fn configured_storage_roots_for_execution(&self) -> Result<Vec<PathBuf>, String> {
        self.configured_storage_authority()
            .map(ServerStorageRoots::into_roots)
    }

    async fn configured_storage_roots_for_execution_async(&self) -> Result<Vec<PathBuf>, String> {
        self.configured_storage_authority_async()
            .await
            .map(ServerStorageRoots::into_roots)
    }

    #[cfg(test)]
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

    async fn configured_storage_authority_async(&self) -> Result<ServerStorageRoots, String> {
        let paths = self
            .run_db("list_storage_roots_for_authority", |db| {
                rt_db::list_storage_roots(db)
                    .map(|roots| {
                        roots
                            .into_iter()
                            .map(|root| PathBuf::from(root.path))
                            .collect::<Vec<_>>()
                    })
                    .map_err(|error| error.to_string())
            })
            .await?;
        ServerStorageRoots::from_configured_paths(paths).map_err(|error| error.to_string())
    }

    /// Make the database authoritative at startup. `Engine::start` normally
    /// receives an empty registry, but tests, embedders, and a failed restart
    /// can hand it an older in-memory projection. Rehydrate every DB row
    /// below rather than allowing stale registry entries or duplicate handles
    /// to survive a restart boundary.
    async fn reconcile_registry_projection(&self, rows: &[TorrentRow]) -> anyhow::Result<()> {
        let persisted = rows
            .iter()
            .map(|row| row.info_hash.as_str())
            .collect::<HashSet<_>>();
        let existing = {
            let registry = self.registry.read().await;
            registry
                .iter()
                .map(|entry| entry.info_hash)
                .collect::<Vec<_>>()
        };
        if existing.is_empty() {
            return Ok(());
        }
        let mut registry = self.registry.write().await;
        for info_hash in existing {
            if let Err(error) = registry.remove(&info_hash) {
                warn!(
                    component = "engine",
                    operation = "reconcile_registry_projection",
                    torrent = %info_hash,
                    result = "error",
                    error = %error,
                    "failed to remove stale in-memory registry entry"
                );
            } else if !persisted.contains(info_hash.as_str()) {
                warn!(
                    component = "engine",
                    operation = "reconcile_registry_projection",
                    torrent = %info_hash,
                    result = "quarantined",
                    "removed in-memory torrent with no durable database row"
                );
            }
        }
        Ok(())
    }

    /// Reconcile the cheap, independently durable projections before restore.
    /// This pass deliberately does not parse every dormant metainfo blob: it
    /// checks row/blob ownership and file-projection existence, records
    /// actionable issues durably, and leaves expensive metadata rebuilds to
    /// the first promotion of the affected torrent.
    async fn reconcile_persisted_projections(&self, rows: &mut [TorrentRow]) -> anyhow::Result<()> {
        let row_hashes = rows
            .iter()
            .map(|row| row.info_hash.clone())
            .collect::<HashSet<_>>();
        let mut issues = self.reconcile_projection_directory(
            &torrent_blob_dir(&self.config),
            &row_hashes,
            "torrent_blob",
            ".torrent",
        )?;
        issues.extend(self.reconcile_projection_directory(
            &fastresume_dir(&self.config),
            &row_hashes,
            "fastresume",
            ".fastresume.json",
        )?);

        let config = Arc::clone(&self.config);
        let mut rows_for_db = rows.to_vec();
        let updated_rows = self
            .run_db("reconcile_torrent_projections", move |db| {
                for issue in &issues {
                    rt_db::record_active_issue(db, issue).map_err(|error| error.to_string())?;
                }
                for row in rows_for_db.iter_mut() {
                    let blob_path = torrent_blob_path(&config, &row.info_hash);
                    if is_metadata_placeholder_row_for(&config, row) {
                        rt_db::resolve_active_issue(
                            db,
                            Some(&row.info_hash),
                            "torrent_blob",
                            Some(&blob_path.to_string_lossy()),
                            unix_now_i64(),
                        )
                        .map_err(|error| error.to_string())?;
                        continue;
                    }
                    if !rt_storage::metadata_no_follow(&blob_path)
                        .is_ok_and(|metadata| metadata.is_file())
                    {
                        let reason = format!(
                            "durable torrent row has no metainfo blob at {}",
                            blob_path.display()
                        );
                        let issue = rt_db::ProjectionIssueRow {
                            issue_id: None,
                            info_hash: Some(row.info_hash.clone()),
                            artifact: "torrent_blob".to_owned(),
                            path: Some(blob_path.to_string_lossy().into_owned()),
                            reason: reason.clone(),
                            detected_at: unix_now_i64(),
                            resolved_at: None,
                        };
                        if row.state != TorrentState::Error.as_str() {
                            row.state = TorrentState::Error.as_str().to_owned();
                            let tx = db.transaction().map_err(|error| error.to_string())?;
                            rt_db::upsert_in_tx(&tx, row).map_err(|error| error.to_string())?;
                            rt_db::record_active_issue_in_tx(&tx, &issue)
                                .map_err(|error| error.to_string())?;
                            tx.commit().map_err(|error| error.to_string())?;
                        } else {
                            rt_db::record_active_issue(db, &issue)
                                .map_err(|error| error.to_string())?;
                        }
                        continue;
                    }
                    rt_db::resolve_active_issue(
                        db,
                        Some(&row.info_hash),
                        "torrent_blob",
                        Some(&blob_path.to_string_lossy()),
                        unix_now_i64(),
                    )
                    .map_err(|error| error.to_string())?;

                    if row.total_length > 0
                        && rt_db::count_torrent_files(db, &row.info_hash)
                            .map_err(|error| error.to_string())?
                            == 0
                    {
                        let path = format!("db://torrent_files/{}", row.info_hash);
                        rt_db::record_active_issue(
                            db,
                            &rt_db::ProjectionIssueRow {
                                issue_id: None,
                                info_hash: Some(row.info_hash.clone()),
                                artifact: "torrent_files".to_owned(),
                                path: Some(path),
                                reason: "durable file projection is empty; it will be rebuilt before promotion"
                                    .to_owned(),
                                detected_at: unix_now_i64(),
                                resolved_at: None,
                            },
                        )
                        .map_err(|error| error.to_string())?;
                    }
                }
                Ok(rows_for_db)
            })
            .await
            .map_err(anyhow::Error::msg)?;
        rows.clone_from_slice(&updated_rows);
        Ok(())
    }

    /// Move an unowned projection artifact out of the live directory instead
    /// of silently deleting it. The quarantine is on the same filesystem so
    /// the rename is atomic and restart-safe.
    fn reconcile_projection_directory(
        &self,
        directory: &Path,
        row_hashes: &HashSet<String>,
        artifact: &str,
        suffix: &str,
    ) -> anyhow::Result<Vec<rt_db::ProjectionIssueRow>> {
        rt_storage::create_dir_all_no_follow(directory)
            .with_context(|| format!("creating projection directory {}", directory.display()))?;
        let mut issues = Vec::new();
        let entries = std::fs::read_dir(directory)
            .with_context(|| format!("reading projection directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(info_hash) = file_name.strip_suffix(suffix) else {
                continue;
            };
            if row_hashes.contains(info_hash) {
                continue;
            }
            let target = self.quarantine_projection_file(&path, directory)?;
            let reason = format!("orphan {artifact} projection moved to {}", target.display());
            issues.push(rt_db::ProjectionIssueRow {
                issue_id: None,
                info_hash: decode_info_hash_bytes(info_hash)
                    .is_ok()
                    .then(|| info_hash.to_owned()),
                artifact: artifact.to_owned(),
                path: Some(path.to_string_lossy().into_owned()),
                reason,
                detected_at: unix_now_i64(),
                resolved_at: None,
            });
        }
        Ok(issues)
    }

    fn quarantine_projection_file(&self, path: &Path, directory: &Path) -> anyhow::Result<PathBuf> {
        let quarantine = directory.join("quarantine");
        rt_storage::create_dir_all_no_follow(&quarantine)?;
        let file_name = path.file_name().ok_or_else(|| {
            anyhow::anyhow!("projection path has no file name: {}", path.display())
        })?;
        let file_name = file_name.to_string_lossy();
        let mut target = quarantine.join(file_name.as_ref());
        let mut collision = 0_u32;
        while target.exists() {
            collision = collision.saturating_add(1);
            target = quarantine.join(format!("{file_name}.{collision}"));
        }
        rt_storage::rename_no_follow(path, &target)
            .with_context(|| format!("quarantining projection artifact {}", path.display()))?;
        Ok(target)
    }

    /// Rebuild a missing normalized file projection from authoritative
    /// metainfo at the point where the torrent is already being promoted or
    /// restored hot. Dormant startup deliberately avoids this parse; the
    /// active issue recorded there is resolved here in one transaction.
    async fn repair_torrent_file_projection(
        &self,
        info_hash: &str,
        meta: &TorrentMeta,
    ) -> anyhow::Result<()> {
        let files = meta_file_rows(info_hash, meta);
        if files.is_empty() {
            return Ok(());
        }
        let info_hash = info_hash.to_owned();
        self.run_db("repair_torrent_file_projection", move |db| {
            if rt_db::count_torrent_files(db, &info_hash).map_err(|error| error.to_string())? == 0 {
                let tx = db.transaction().map_err(|error| error.to_string())?;
                rt_db::replace_torrent_files_in_tx(&tx, &info_hash, &files)
                    .map_err(|error| error.to_string())?;
                let path = format!("db://torrent_files/{info_hash}");
                rt_db::resolve_active_issue_in_tx(
                    &tx,
                    Some(&info_hash),
                    "torrent_files",
                    Some(&path),
                    unix_now_i64(),
                )
                .map_err(|error| error.to_string())?;
                tx.commit().map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    async fn restore_persisted_error_projection(
        &mut self,
        row: &TorrentRow,
        artifact: &str,
        artifact_path: &Path,
        reason: String,
        quarantine: bool,
    ) -> anyhow::Result<()> {
        let quarantined_path = if quarantine
            && rt_storage::metadata_no_follow(artifact_path)
                .is_ok_and(|metadata| metadata.is_file())
        {
            Some(self.quarantine_projection_file(
                artifact_path,
                artifact_path.parent().unwrap_or_else(|| Path::new(".")),
            )?)
        } else {
            None
        };
        let mut error_row = row.clone();
        error_row.state = TorrentState::Error.as_str().to_owned();
        let issue_reason = quarantined_path
            .as_ref()
            .map(|path| format!("{reason}; moved to quarantine at {}", path.display()))
            .unwrap_or_else(|| reason.clone());
        let restored_event = self.session_event_row(
            Some(&row.info_hash),
            EVENT_TORRENT_RESTORED,
            Some("torrent restored in error state after projection reconciliation"),
            serde_json::json!({
                "state": TorrentState::Error.as_str(),
                "artifact": artifact,
                "reason": issue_reason,
            }),
        );
        let error_row_for_db = error_row.clone();
        let issue = rt_db::ProjectionIssueRow {
            issue_id: None,
            info_hash: Some(row.info_hash.clone()),
            artifact: artifact.to_owned(),
            path: Some(artifact_path.to_string_lossy().into_owned()),
            reason: issue_reason.clone(),
            detected_at: unix_now_i64(),
            resolved_at: None,
        };
        let restored_event_for_db = restored_event.clone();
        let retention = self.config.logging.event_retention;
        self.run_db("restore_error_projection", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_in_tx(&tx, &error_row_for_db).map_err(|error| error.to_string())?;
            rt_db::record_active_issue_in_tx(&tx, &issue).map_err(|error| error.to_string())?;
            rt_db::append_session_event_in_tx(&tx, &restored_event_for_db)
                .map_err(|error| error.to_string())?;
            rt_db::prune_session_events_in_tx(&tx, retention).map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)?;
        self.registry
            .write()
            .await
            .add_dormant(dormant_entry_from_row(&error_row))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        self.runtime.tier_controller.apply_input(
            row.info_hash.clone(),
            TierInput {
                state: TorrentState::Error,
                connected_peers: 0,
                outstanding_requests: 0,
                inbound_peer: false,
                tracker_due: false,
                last_active: None,
                now: Instant::now(),
            },
        );
        self.runtime.tier_controller.set_dormant_snapshot(
            row.info_hash.clone(),
            dormant_snapshot_from_row(&error_row, TorrentState::Error, None),
        );
        Ok(())
    }

    #[cfg(test)]
    fn authorize_storage_path(&self, path: &Path) -> Result<(), String> {
        let roots = self.configured_storage_roots_for_execution()?;
        let authority =
            ServerStorageRoots::from_configured_paths(roots).map_err(|e| e.to_string())?;
        authority.authorize_path(path).map_err(|e| e.to_string())
    }

    async fn authorize_storage_path_async(&self, path: &Path) -> Result<(), String> {
        let authority = self.configured_storage_authority_async().await?;
        authority
            .authorize_path(path)
            .map_err(|error| error.to_string())
    }

    async fn repair_missing_torrent_tracker_rows_async(
        &self,
        rows: &[TorrentRow],
    ) -> anyhow::Result<()> {
        let rows = rows.to_vec();
        self.run_db("repair_missing_torrent_tracker_rows", move |db| {
            let existing = rt_db::list_all_torrent_trackers(db)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|tracker| tracker.info_hash)
                .collect::<HashSet<_>>();
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
                rt_db::replace_torrent_trackers(db, &row.info_hash, &trackers)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(anyhow::Error::msg)
    }

    #[allow(dead_code)]
    #[cfg(test)]
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

    /// Commit a job projection and all events describing that update as one
    /// SQLite transaction. A job state without its event is not an auditable
    /// state transition and can make restart recovery choose the wrong path.
    async fn persist_job_with_events_async(
        &self,
        job: &rt_db::JobRow,
        events: &[rt_db::JobEventRow],
    ) -> Result<(), String> {
        let job = job.clone();
        let events = events.to_vec();
        self.run_db("persist_job_with_events", move |db| {
            let tx = db.transaction().map_err(|error| error.to_string())?;
            rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
            for event in &events {
                rt_db::append_job_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())
        })
        .await
    }

    #[cfg(test)]
    fn persist_job_with_events(
        &self,
        job: &rt_db::JobRow,
        events: &[rt_db::JobEventRow],
    ) -> Result<(), String> {
        let mut db = self.db.lock().expect("database mutex poisoned");
        let tx = db.transaction().map_err(|error| error.to_string())?;
        rt_db::upsert_job_in_tx(&tx, job).map_err(|error| error.to_string())?;
        for event in events {
            rt_db::append_job_event_in_tx(&tx, event).map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())
    }

    async fn update_job_state_async(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        message: Option<&str>,
    ) -> Result<(), String> {
        let job_id_for_db = job_id.to_owned();
        let mut job = self
            .run_db("load_job_for_state_update", move |db| {
                rt_db::get_job(db, &job_id_for_db).map_err(|error| error.to_string())
            })
            .await?;
        let now = unix_now_i64();
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
        let started = (state == JOB_STATE_RUNNING).then(|| rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: "check_started".to_owned(),
            message: Some("recheck started".to_owned()),
            payload: serde_json::json!({ "state": state }).to_string(),
        });
        let mut events = vec![event];
        if let Some(started) = started {
            events.push(started);
        }
        self.persist_job_with_events_async(&job, &events).await
    }

    async fn update_job_state_best_effort(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        message: Option<&str>,
    ) {
        if let Err(update_error) = self
            .update_job_state_async(job_id, state, error, message)
            .await
        {
            warn!(
                component = "db",
                operation = "persist_job_state",
                job_id,
                state,
                result = "error",
                error = %update_error,
                "failed to persist best-effort job state transition"
            );
        }
    }

    #[cfg(test)]
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
        let started = (state == JOB_STATE_RUNNING).then(|| rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: "check_started".to_owned(),
            message: Some("recheck started".to_owned()),
            payload: serde_json::json!({ "state": state }).to_string(),
        });
        let mut events = vec![event];
        if let Some(started) = started {
            events.push(started);
        }
        if let Err(e) = self.persist_job_with_events(&job, &events) {
            warn!(
                component = "db",
                operation = "persist_job_state",
                job_id,
                state,
                result = "error",
                error = %e,
                "failed to persist job state and event atomically"
            );
        }
    }

    async fn persist_pure_v2_recheck_job_async(
        &self,
        job_id: &str,
        done: i64,
        total: i64,
        invalid_files: &[i64],
    ) -> CmdResult<()> {
        let job_id_for_db = job_id.to_owned();
        let mut job = self
            .run_db("load_pure_v2_recheck_job", move |db| {
                rt_db::get_job(db, &job_id_for_db).map_err(|error| {
                    format!("failed to load pure v2 recheck job {job_id_for_db}: {error}")
                })
            })
            .await?;
        let now = unix_now_i64();
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
        self.persist_job_with_events_async(&job, &[event])
            .await
            .map_err(|error| {
                format!(
                    "failed to persist pure v2 recheck job {job_id} and completion event: {error}"
                )
            })
    }

    #[allow(dead_code)]
    #[cfg(test)]
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

    #[cfg(test)]
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
            total: db_i64_usize(plan.steps.len()),
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
        self.persist_job_with_events(&job, &[event])?;
        Ok(job_id)
    }

    async fn create_storage_plan_job_with_context_async(
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
            total: db_i64_usize(plan.steps.len()),
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
        self.persist_job_with_events_async(&job, &[event]).await?;
        Ok(job_id)
    }

    async fn queue_storage_plan_job_with_context(
        &self,
        operation: &str,
        affected_torrents: Vec<String>,
        plan: &StoragePlan,
        completed_steps: Vec<usize>,
        context: serde_json::Value,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<String, String> {
        let server_roots = self.configured_storage_roots_for_execution_async().await?;
        let completed_steps = normalize_storage_plan_completed_steps(plan, completed_steps)?;
        let job_id = self
            .create_storage_plan_job_with_context_async(operation, affected_torrents, plan, context)
            .await?;
        let submit_result = {
            #[cfg(not(test))]
            {
                self.services.storage_jobs.submit_managed(
                    job_id.clone(),
                    operation.to_owned(),
                    plan.clone(),
                    completed_steps,
                    server_roots,
                    completion,
                )
            }
            #[cfg(test)]
            {
                self.services.storage_jobs.submit(
                    Arc::clone(&self.db),
                    job_id.clone(),
                    operation.to_owned(),
                    plan.clone(),
                    completed_steps,
                    server_roots,
                    completion,
                )
            }
        };
        if let Err(error) = submit_result {
            self.update_job_state_best_effort(
                &job_id,
                JOB_STATE_FAILED,
                Some(error.clone()),
                Some("storage plan could not be queued"),
            )
            .await;
            return Err(error);
        }
        Ok(job_id)
    }

    #[allow(dead_code)]
    #[cfg(test)]
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
    #[cfg(test)]
    fn persist_storage_plan_checkpoint(
        &self,
        job_id: &str,
        operation: &str,
        plan: &StoragePlan,
        completed_steps: &[usize],
    ) -> Result<(), String> {
        let now = unix_now_i64();
        let done = db_i64_usize(completed_steps.len());
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
                .map(|step| db_i64(step.bytes))
                .fold(0_i64, i64::saturating_add),
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
        self.persist_job_with_events(&job, &[event])
    }

    #[allow(dead_code)]
    #[cfg(test)]
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
        job.done = db_i64_usize(completed_steps.len());
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
        self.persist_job_with_events(&job, &[event])
    }
}

fn engine_stats_from_registry(registry_stats: rt_session::SessionRegistryStats) -> EngineStats {
    EngineStats {
        torrents_total: registry_stats.torrents_total,
        torrents_seeding: registry_stats.torrents_seeding,
        torrents_downloading: registry_stats.torrents_downloading,
        torrents_paused: registry_stats
            .torrents_stopped
            .saturating_add(registry_stats.torrents_paused),
        torrents_checking: registry_stats.torrents_checking,
        torrents_queued: registry_stats.torrents_queued,
        torrents_error: registry_stats.torrents_error,
        torrents_metadata_pending: registry_stats.torrents_metadata_pending,
        bytes_uploaded: registry_stats.bytes_uploaded,
        bytes_downloaded: registry_stats.bytes_downloaded,
        bytes_left: registry_stats.bytes_left,
        ..Default::default()
    }
}

fn apply_activity_tier_stats(
    stats: &mut EngineStats,
    [dormant, warm, hot]: [usize; 3],
    torrents_total: u64,
    dormant_runtime_heap_bytes: u64,
) {
    let tracked = dormant.saturating_add(warm).saturating_add(hot);
    stats.torrents_activity_dormant =
        dormant as u64 + torrents_total.saturating_sub(tracked as u64);
    stats.torrents_activity_warm = warm as u64;
    stats.torrents_activity_hot = hot as u64;
    stats.dormant_runtime_heap_bytes = dormant_runtime_heap_bytes;
}

fn apply_storage_job_stats(
    stats: &mut EngineStats,
    inflight: u64,
    queue_depth: u64,
    capacity: u64,
    workers: u64,
    workers_healthy: u64,
) {
    stats.storage_jobs_inflight = inflight;
    stats.storage_jobs_queue_depth = queue_depth;
    stats.storage_jobs_capacity = capacity;
    stats.storage_workers = workers;
    stats.storage_workers_healthy = workers_healthy;
}

fn finalize_engine_stats_resources(
    stats: &mut EngineStats,
    mut resources: ResourceSnapshot,
    pressure_constrained_pct: u8,
    pressure_critical_pct: u8,
) {
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
        pressure_constrained_pct,
        pressure_critical_pct,
    );
    stats.resources = Some(resources);
}

async fn collect_engine_stats_background(input: EngineStatsRefreshInput) -> CmdResult<EngineStats> {
    let registry_stats = input.registry.read().await.stats();
    let mut stats = engine_stats_from_registry(registry_stats);
    let task_count = input.task_channels.len() as u64;

    let (jobs, tracker_counts) = input
        .db
        .run("collect_engine_stats", |db| {
            let jobs = rt_db::count_active_jobs(db).map_err(|e| e.to_string())?;
            let trackers = rt_db::torrent_tracker_status_counts(db).map_err(|e| e.to_string())?;
            Ok((jobs, trackers))
        })
        .await
        .map_err(|error| format!("database stats unavailable: {error}"))?;
    stats.jobs_active = jobs;
    stats.trackers_total = tracker_counts.total;
    stats.trackers_working = tracker_counts.working;
    stats.trackers_warning = tracker_counts.warning;
    stats.trackers_error = tracker_counts.error;
    apply_storage_job_stats(
        &mut stats,
        input.storage_jobs_inflight,
        input.storage_jobs_queue_depth,
        input.storage_jobs_capacity,
        input.storage_workers,
        input.storage_workers_healthy,
    );

    if let Some(dht_tx) = input.dht_tx {
        let (reply, rx) = tokio::sync::oneshot::channel();
        if dht_tx.try_send(DhtCommand::GetStats { reply }).is_ok() {
            if let Ok(Ok(dht)) = timeout(Duration::from_millis(250), rx).await {
                stats.dht_routing_nodes = dht.routing_nodes;
                stats.dht_announced_peer_sets = dht.announced_peer_sets;
                stats.dht_announced_peers = dht.announced_peers;
                stats.dht_tracked_torrents = dht.tracked_torrents;
                stats.dht_outstanding_requests = dht.outstanding_requests;
                stats.dht_queried_nodes = dht.queried_nodes;
            }
        }
    }

    let runtime_results = timeout(
        ENGINE_STATS_TASK_QUERY_DEADLINE,
        stream::iter(
            input
                .task_channels
                .into_iter()
                .map(|(info_hash, tx)| async move {
                    let (reply, rx) = tokio::sync::oneshot::channel();
                    let send_result = timeout(
                        ENGINE_COMMAND_SEND_TIMEOUT,
                        tx.send(TorrentCmd::GetRuntimeStats { reply }),
                    )
                    .await;
                    if !matches!(send_result, Ok(Ok(()))) {
                        return (info_hash, None);
                    }
                    match timeout(ENGINE_STATS_TASK_QUERY_DEADLINE, rx).await {
                        Ok(Ok(runtime)) => (info_hash, Some(runtime)),
                        Ok(Err(_)) | Err(_) => (info_hash, None),
                    }
                }),
        )
        .buffer_unordered(64)
        .collect::<Vec<_>>(),
    )
    .await
    .unwrap_or_default();
    for (info_hash, runtime) in runtime_results {
        if let Some(runtime) = runtime {
            stats.add_torrent_runtime(info_hash, runtime);
        }
    }
    stats.torrent_tasks_active = task_count;
    apply_activity_tier_stats(
        &mut stats,
        input.tier_counts,
        registry_stats.torrents_total,
        input.dormant_runtime_heap_bytes,
    );
    finalize_engine_stats_resources(
        &mut stats,
        input.resources.snapshot(),
        input.pressure_constrained_pct,
        input.pressure_critical_pct,
    );
    Ok(stats)
}

#[allow(dead_code)]
fn decode_storage_plan_event(
    payload: &str,
) -> Option<(String, StoragePlan, Option<Vec<usize>>, serde_json::Value)> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let operation = value.get("operation")?.as_str()?.to_owned();
    let plan = serde_json::from_value(value.get("plan")?.clone()).ok()?;
    let completed_steps = match value.get("completed_steps") {
        None => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    let context = value
        .get("context")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some((operation, plan, completed_steps, context))
}

fn decode_storage_plan_context(payload: &str) -> Option<serde_json::Value> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    value.get("context").cloned()
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

fn normalize_storage_plan_targets(mut targets: Vec<String>) -> Result<Vec<String>, String> {
    if targets.len() > MAX_STORAGE_PLAN_AFFECTED_TORRENTS {
        return Err(format!(
            "storage plan affects too many torrents (maximum {})",
            MAX_STORAGE_PLAN_AFFECTED_TORRENTS
        ));
    }
    for target in &mut targets {
        *target = target.trim().to_owned();
        if target.is_empty() {
            return Err("storage plan affected torrent hash must not be empty".to_owned());
        }
    }
    targets.sort_unstable();
    targets.dedup();
    Ok(targets)
}

fn recovered_storage_plan_steps(
    plan: &StoragePlan,
    checkpoint: i64,
    event_completed_steps: Option<Vec<usize>>,
) -> Result<Vec<usize>, String> {
    // The durable job projection stores a count, not the step indexes. Prefer
    // the event's exact sparse list, including an explicitly empty list; only
    // use the prefix fallback for a crash between the job-row update and its
    // checkpoint-event insert.
    if let Some(event_completed_steps) = event_completed_steps {
        return normalize_storage_plan_completed_steps(plan, event_completed_steps);
    }
    if checkpoint < 0 {
        return Err("storage job checkpoint must not be negative".to_owned());
    }
    let checkpoint = checkpoint as usize;
    if checkpoint > plan.steps.len() {
        return Err(format!(
            "storage job checkpoint {checkpoint} is outside plan length {}",
            plan.steps.len()
        ));
    }
    Ok((0..checkpoint).collect())
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

/// Convert the engine's unsigned counters into SQLite's signed INTEGER
/// representation without allowing a large, valid torrent or long-lived
/// transfer counter to wrap into a negative projection.
fn db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn db_i64_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn row_from_entry(entry: &TorrentEntry, meta: &TorrentMeta) -> TorrentRow {
    TorrentRow {
        info_hash: entry.info_hash.clone(),
        name: entry.name.clone(),
        total_length: db_i64(meta_total_length(meta)),
        piece_length: db_i64(meta_piece_length(meta)),
        piece_count: db_i64_usize(meta_piece_count(meta)),
        is_private: meta.is_private(),
        save_path: entry.save_path.clone(),
        category: entry.category.clone(),
        tags: entry.tags.clone(),
        state: entry.state.as_str().to_owned(),
        added_at: db_i64(entry.added_at),
        completed_at: entry.completed_at.map(db_i64),
        uploaded: db_i64(entry.stats.uploaded),
        downloaded: db_i64(entry.stats.downloaded),
        ratio: entry.stats.ratio(),
        trackers: meta_all_trackers(meta),
    }
}

#[cfg(test)]
fn persist_torrent_files(
    db: &mut Connection,
    info_hash: &str,
    meta: &TorrentMeta,
) -> anyhow::Result<()> {
    let rows = meta_file_rows(info_hash, meta);
    rt_db::replace_torrent_files(db, info_hash, &rows)?;
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
            tracker_index: db_i64_usize(idx),
            tier: db_i64_usize(idx),
            url: url.clone(),
            tracker_id: None,
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

fn dormant_entry_from_row(row: &TorrentRow) -> DormantTorrent {
    entry_from_row(row).into_dormant()
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

fn tier_policy_from_config(config: &Config) -> TierPolicy {
    TierPolicy {
        hot_idle: Duration::from_secs(config.runtime.tier_hot_idle_secs),
        warm_idle: Duration::from_secs(config.runtime.tier_warm_idle_secs),
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
    dormant_snapshot_from_fields(&row.info_hash, state, tracker_deadline)
}

fn dormant_snapshot_from_fields(
    info_hash: &str,
    state: TorrentState,
    tracker_deadline: Option<Instant>,
) -> DormantTorrentSnapshot {
    DormantTorrentSnapshot::new(info_hash.to_owned(), state, tracker_deadline, None)
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

fn engine_tracker_health(row: rt_db::TorrentTrackerHealthRow) -> EngineTrackerHealth {
    EngineTrackerHealth {
        tracker: row.tracker,
        torrent_count: row.torrent_count,
        active_count: row.active_count,
        complete_count: row.complete_count,
        error_count: row.error_count,
        seed_count: row.seed_count,
        peer_count: row.peer_count,
        last_updated: row.last_updated,
    }
}

fn torrent_blob_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("torrents")
}

fn torrent_blob_path(config: &Config, info_hash: &str) -> PathBuf {
    torrent_blob_dir(config).join(format!("{info_hash}.torrent"))
}

fn save_torrent_blob_from_config(
    config: &Config,
    info_hash: &str,
    raw: &[u8],
) -> anyhow::Result<()> {
    let path = torrent_blob_path(config, info_hash);
    if let Some(parent) = path.parent() {
        rt_storage::create_dir_all_no_follow(parent)?;
    }
    rt_storage::write_file_no_follow(&path, raw)?;
    Ok(())
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

fn meta_file_rows(info_hash: &str, meta: &TorrentMeta) -> Vec<rt_db::TorrentFileRow> {
    match meta {
        TorrentMeta::V1(meta) => meta
            .files
            .iter()
            .map(|file| rt_db::TorrentFileRow {
                info_hash: info_hash.to_owned(),
                file_index: i64::from(file.index),
                path: file.path.as_display(),
                length: db_i64(file.length),
                offset: db_i64(file.offset),
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
                file_index: i64::from(file.index),
                path: file.path.as_display(),
                length: db_i64(file.length),
                offset: db_i64(file.offset),
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
                file_index: i64::from(file.index),
                path: file.path.as_display(),
                length: db_i64(file.length),
                offset: db_i64(file.offset),
                priority: 1,
                wanted: true,
                completed_bytes: 0,
            })
            .collect(),
    }
}

fn file_entries_from_rows(
    rows: &[rt_db::TorrentFileRow],
) -> CmdResult<Vec<(rt_path::SafeRelPath, u64)>> {
    rows.iter()
        .map(|row| {
            let components = row.path.split('/').collect::<Vec<_>>();
            let path = rt_path::SafeRelPath::from_components(&components, cfg!(windows)).map_err(
                |error| {
                    format!(
                        "invalid persisted torrent file path {:?}: {error}",
                        row.path
                    )
                },
            )?;
            let length = u64::try_from(row.length)
                .map_err(|_| format!("invalid negative torrent file length for {:?}", row.path))?;
            Ok((path, length))
        })
        .collect()
}

/// Build a move plan from durable file metadata without borrowing the engine
/// actor. Every filesystem probe in this function is intentionally synchronous
/// because callers run it on the blocking storage-planning task.
fn plan_torrent_payload_files_with_authority(
    authority: &ServerStorageRoots,
    source_root: &Path,
    destination_root: &Path,
    file_entries: &[(rt_path::SafeRelPath, u64)],
) -> CmdResult<Option<StoragePlan>> {
    authority
        .authorize_path(source_root)
        .map_err(|error| error.to_string())?;
    authority
        .authorize_path(destination_root)
        .map_err(|error| error.to_string())?;
    let mut steps = Vec::new();
    let mut rollback_steps = Vec::new();
    let mut issues = Vec::new();
    for (rel_path, bytes) in file_entries {
        let source = rel_path.resolve(source_root);
        if !source.exists() {
            continue;
        }
        let destination = rel_path.resolve(destination_root);
        authority
            .authorize_path(&source)
            .map_err(|error| error.to_string())?;
        authority
            .authorize_path(&destination)
            .map_err(|error| error.to_string())?;
        let plan = rt_storage::plan_move(&rt_storage::MovePlanRequest {
            source,
            destination,
            bytes: *bytes,
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
    Ok(Some(StoragePlan {
        dry_run: false,
        can_apply: issues.is_empty(),
        issues,
        steps,
        rollback_steps,
    }))
}

/// Read and parse the authoritative metainfo needed to reconstruct a dormant
/// v1-capable torrent. Every filesystem operation and the potentially large
/// bencode parse run on the blocking promotion worker; the engine actor only
/// installs the prepared task after this function returns.
fn prepare_torrent_task_from_storage(
    config: &Config,
    db: &DbExecutor,
    info_hash: &str,
) -> CmdResult<PreparedTorrentTaskData> {
    let info_hash_for_db = info_hash.to_owned();
    let row = db.run_blocking("promote_load_torrent", move |db| {
        rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())
    })?;
    if is_metadata_placeholder_row_for(config, &row) {
        return Err(format!("torrent {info_hash} is waiting for metadata"));
    }

    let storage_roots = db.run_blocking("promote_load_storage_roots", |db| {
        let roots = rt_db::list_storage_roots(db).map_err(|error| error.to_string())?;
        Ok(roots
            .into_iter()
            .map(|root| PathBuf::from(root.path))
            .collect::<Vec<_>>())
    })?;
    let authority = ServerStorageRoots::from_configured_paths(storage_roots)
        .map_err(|error| error.to_string())?;
    let save_path = PathBuf::from(&row.save_path);
    authority
        .authorize_path(&save_path)
        .map_err(|error| error.to_string())?;

    let raw =
        load_torrent_blob_from_config(config, info_hash).map_err(|error| error.to_string())?;
    let meta = parse_torrent(&raw).map_err(|error| error.to_string())?;
    let files = meta_file_rows(info_hash, &meta);
    let is_private = meta.is_private();
    let Some(v1) = meta_v1(meta) else {
        return Err("pure v2 peer transfer is not implemented".to_owned());
    };
    if hex::encode(v1.info_hash) != info_hash {
        return Err(format!(
            "torrent metadata info hash {} does not match persisted row {info_hash}",
            hex::encode(v1.info_hash)
        ));
    }
    if !files.is_empty() {
        let info_hash_for_db = info_hash.to_owned();
        db.run_blocking("promote_repair_file_projection", move |db| {
            if rt_db::count_torrent_files(db, &info_hash_for_db)
                .map_err(|error| error.to_string())?
                == 0
            {
                rt_db::replace_torrent_files(db, &info_hash_for_db, &files)
                    .map_err(|error| error.to_string())?;
                let path = format!("db://torrent_files/{info_hash_for_db}");
                rt_db::resolve_active_issue(
                    db,
                    Some(&info_hash_for_db),
                    "torrent_files",
                    Some(&path),
                    unix_now_i64(),
                )
                .map_err(|error| error.to_string())?;
            }
            Ok(())
        })?;
    }
    Ok(PreparedTorrentTaskData {
        info_hash: v1.info_hash,
        meta: v1,
        save_path,
        is_private,
    })
}

fn load_torrent_blob_from_config(config: &Config, info_hash: &str) -> anyhow::Result<Vec<u8>> {
    let blob_path = torrent_blob_path(config, info_hash);
    rt_storage::read_file_no_follow_limited(&blob_path, MAX_TORRENT_BYTES)
        .with_context(|| format!("reading persisted torrent blob {}", blob_path.display()))
}

fn load_torrent_metadata_from_sources(
    config: &Config,
    db: &DbExecutor,
    info_hash: &str,
) -> anyhow::Result<EngineTorrentMetadata> {
    let blob_path = torrent_blob_path(config, info_hash);
    let raw = match rt_storage::read_file_no_follow_limited(&blob_path, MAX_TORRENT_BYTES) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let info_hash_for_db = info_hash.to_owned();
            let row = db
                .run_blocking("metadata_load_placeholder", move |db| {
                    rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())
                })
                .map_err(anyhow::Error::msg)?;
            if is_metadata_placeholder_row_for(config, &row) {
                return Ok(metadata_from_placeholder_row(&row));
            }
            return Err(error).with_context(|| {
                format!("reading persisted torrent metadata {}", blob_path.display())
            });
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("reading persisted torrent metadata {}", blob_path.display())
            });
        }
    };
    let meta = parse_torrent(&raw)?;
    let mut metadata = metadata_from_meta(&meta);
    let info_hash_for_db = info_hash.to_owned();
    let (files, row) = db
        .run_blocking("metadata_load_projection", move |db| {
            Ok((
                rt_db::list_torrent_files(db, &info_hash_for_db)
                    .map_err(|error| error.to_string())?,
                rt_db::get(db, &info_hash_for_db).map_err(|error| error.to_string())?,
            ))
        })
        .map_err(anyhow::Error::msg)?;
    if !files.is_empty() {
        let policy = files
            .into_iter()
            .map(|file| {
                let index = u32::try_from(file.file_index).map_err(|_| {
                    anyhow::anyhow!(
                        "persisted file index {} is outside the engine file-index range",
                        file.file_index
                    )
                })?;
                Ok::<_, anyhow::Error>((index, (file.path, file.priority, file.wanted)))
            })
            .collect::<anyhow::Result<HashMap<_, _>>>()?;
        for file in &mut metadata.files {
            if let Some((path, priority, wanted)) = policy.get(&file.index) {
                file.path = path.clone();
                file.priority = *priority;
                file.wanted = *wanted;
            }
        }
    }
    if !row.trackers.is_empty() {
        metadata.trackers = row.trackers;
    }
    if let Ok(hash) = decode_info_hash_bytes(info_hash) {
        if let Ok(state) = FastresumeStore::new(fastresume_dir(config)).load(info_hash) {
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

struct PureV2RecheckOutput {
    total_length: u64,
    total_files: i64,
    done: i64,
    invalid_files: Vec<i64>,
}

async fn execute_pure_v2_recheck(
    config: Arc<Config>,
    resources: ResourceGovernor,
    authority: ServerStorageRoots,
    save_root: PathBuf,
    info_hash: String,
) -> Result<PureV2RecheckOutput, String> {
    let planning_config = Arc::clone(&config);
    let planning_save_root = save_root.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        authority
            .authorize_path(&planning_save_root)
            .map_err(|error| error.to_string())?;
        let raw = load_torrent_blob_from_config(&planning_config, &info_hash)
            .map_err(|error| error.to_string())?;
        match parse_torrent(&raw).map_err(|error| error.to_string())? {
            TorrentMeta::V2(meta) => Ok(meta),
            _ => Err("pure v2 recheck received non-v2 metadata".to_owned()),
        }
    })
    .await
    .map_err(|error| format!("pure v2 metadata worker failed: {error}"))??;

    let files = parsed
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
            resources: Some(resources),
            storage_io: storage_io_config_from_config(&config),
            ..Default::default()
        },
    );
    let results = V2FileVerifier::new(&save_root, &scheduler, &files)
        .verify_all()
        .await;
    let invalid_files = results
        .iter()
        .filter_map(|(file_index, result)| {
            (!matches!(result, VerifyResult::Valid)).then_some(i64::from(*file_index))
        })
        .collect::<Vec<_>>();
    Ok(PureV2RecheckOutput {
        total_length: parsed.total_length(),
        total_files: db_i64_usize(files.len()),
        done: db_i64_usize(results.len()),
        invalid_files,
    })
}

fn fastresume_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("fastresume")
}

fn register_configured_storage(conn: &Connection, config: &Config) -> anyhow::Result<()> {
    rt_storage::create_dir_all_no_follow(&config.storage.download_dir)?;
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

fn canonical_info_hash(info_hash: String) -> String {
    if matches!(info_hash.len(), 40 | 64) && info_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        info_hash.to_ascii_lowercase()
    } else {
        info_hash
    }
}

pub(crate) fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn unix_deadline_to_instant(deadline: i64, now_unix: i64, now: Instant) -> Option<Instant> {
    let delay = u64::try_from(deadline.saturating_sub(now_unix).max(0)).ok()?;
    now.checked_add(Duration::from_secs(delay))
}

fn setting_i64_checked(conn: &Connection, key: &str) -> CmdResult<i64> {
    match rt_db::get_setting(conn, key) {
        Ok(value) => {
            let parsed = value
                .parse::<i64>()
                .map_err(|error| format!("invalid persisted integer setting {key}: {error}"))?;
            if parsed < 0 {
                Err(format!(
                    "invalid persisted non-negative integer setting {key}: {parsed}"
                ))
            } else {
                Ok(parsed)
            }
        }
        Err(rt_db::DbError::NotFound(_)) => Ok(0),
        Err(error) => Err(error.to_string()),
    }
}

fn setting_bool_checked(conn: &Connection, key: &str) -> CmdResult<bool> {
    match rt_db::get_setting(conn, key) {
        Ok(value) if matches!(value.as_str(), "1" | "true") => Ok(true),
        Ok(value) if matches!(value.as_str(), "0" | "false") => Ok(false),
        Ok(value) => Err(format!("invalid persisted boolean setting {key}: {value}")),
        Err(rt_db::DbError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn setting_bool_with_default_checked(
    conn: &Connection,
    key: &str,
    default: bool,
) -> CmdResult<bool> {
    match rt_db::get_setting(conn, key) {
        Ok(value) if matches!(value.as_str(), "1" | "true") => Ok(true),
        Ok(value) if matches!(value.as_str(), "0" | "false") => Ok(false),
        Ok(value) => Err(format!("invalid persisted boolean setting {key}: {value}")),
        Err(rt_db::DbError::NotFound(_)) => Ok(default),
        Err(error) => Err(error.to_string()),
    }
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

fn persisted_global_tags(conn: &Connection) -> CmdResult<Vec<String>> {
    match rt_db::get_setting(conn, SETTING_GLOBAL_TAGS) {
        Ok(value) => serde_json::from_str::<Vec<String>>(&value)
            .map(normalize_tags)
            .map_err(|error| format!("invalid persisted global tags: {error}")),
        Err(rt_db::DbError::NotFound(_)) => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
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

fn is_metadata_placeholder_row_for(config: &Config, row: &TorrentRow) -> bool {
    if state_from_str(&row.state) == TorrentState::MetadataPending {
        return true;
    }
    state_from_str(&row.state) == TorrentState::Paused
        && row.total_length == 0
        && row.piece_count == 0
        && rt_storage::metadata_no_follow(&torrent_blob_path(config, &row.info_hash)).is_err()
}

fn decode_info_hash_bytes(info_hash: &str) -> anyhow::Result<Vec<u8>> {
    let bytes = hex::decode(info_hash)?;
    match bytes.len() {
        20 | 32 => Ok(bytes),
        len => anyhow::bail!("expected 20-byte or 32-byte info hash, got {len} bytes"),
    }
}

#[cfg(test)]
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
    use crate::TorrentActivityTier;
    use rt_bencode::{encode, BValue};
    use rt_hash::{merkle_root, BlockHash};
    use rt_metainfo::TorrentFileV1;
    use rt_path::SafeRelPath;
    use std::future;

    fn test_resource_governor() -> ResourceGovernor {
        ResourceGovernor::new(ResourceGovernorConfig::default())
    }

    fn persist_test_torrent(engine: &Engine, info_hash: &str) {
        let db = engine.db.lock().unwrap();
        rt_db::upsert(
            &db,
            &TorrentRow {
                info_hash: info_hash.to_owned(),
                name: "test.torrent".to_owned(),
                total_length: 100,
                piece_length: 10,
                piece_count: 10,
                is_private: false,
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
        let handle = EngineHandle {
            tx,
            alive: Arc::new(AtomicBool::new(true)),
            task: Arc::new(EngineTaskControl {
                abort: None,
                shutdown_timeout: Duration::from_secs(1),
                peer_listener_healthy: None,
            }),
        };
        assert!(handle.is_alive());
        drop(rx);
        assert!(!handle.is_alive());
    }

    #[test]
    fn engine_handle_reports_peer_listener_health_separately() {
        let (tx, _rx) = mpsc::channel(1);
        let peer_listener_healthy = Arc::new(AtomicBool::new(false));
        let handle = EngineHandle {
            tx,
            alive: Arc::new(AtomicBool::new(true)),
            task: Arc::new(EngineTaskControl {
                abort: None,
                shutdown_timeout: Duration::from_secs(1),
                peer_listener_healthy: Some(Arc::clone(&peer_listener_healthy)),
            }),
        };
        assert!(!handle.peer_listener_healthy());
        peer_listener_healthy.store(true, Ordering::Release);
        assert!(handle.peer_listener_healthy());
    }

    #[tokio::test]
    async fn engine_health_reply_is_bounded_when_actor_stops_replying() {
        let (tx, mut rx) = mpsc::channel(1);
        let handle = EngineHandle {
            tx,
            alive: Arc::new(AtomicBool::new(true)),
            task: Arc::new(EngineTaskControl {
                abort: None,
                shutdown_timeout: Duration::from_secs(1),
                peer_listener_healthy: None,
            }),
        };
        let actor = tokio::spawn(async move {
            let command = rx.recv().await;
            std::mem::forget(command);
            future::pending::<()>().await;
        });

        let result = handle.subsystem_health().await;
        assert_eq!(result, Err("engine health command timed out".to_owned()));
        actor.abort();
    }

    #[tokio::test]
    async fn engine_command_send_is_bounded_when_mailbox_is_full() {
        let (tx, _rx) = mpsc::channel(1);
        let (reply, _reply_rx) = oneshot::channel();
        tx.try_send(EngineCmd::GetHealth { reply })
            .expect("test mailbox should accept its first command");
        let handle = EngineHandle {
            tx,
            alive: Arc::new(AtomicBool::new(true)),
            task: Arc::new(EngineTaskControl {
                abort: None,
                shutdown_timeout: Duration::from_secs(1),
                peer_listener_healthy: None,
            }),
        };

        let result = handle.subsystem_health().await;
        assert_eq!(result, Err("engine command queue timed out".to_owned()));
    }

    #[tokio::test]
    async fn engine_handle_liveness_drops_on_actor_panic() {
        let (tx, _rx) = mpsc::channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let handle = EngineHandle {
            tx,
            alive: Arc::clone(&alive),
            task: Arc::new(EngineTaskControl {
                abort: None,
                shutdown_timeout: Duration::from_secs(1),
                peer_listener_healthy: None,
            }),
        };
        let peer_listener_stop = watch::channel(false).0;
        let task = tokio::spawn(async move {
            let _liveness = EngineLivenessGuard {
                alive,
                peer_listener_stop,
            };
            panic!("injected engine actor failure");
        });

        assert!(task.await.is_err());
        assert!(!handle.is_alive());
    }

    #[tokio::test]
    async fn engine_handle_shutdown_aborts_an_actor_stuck_after_accepting_command() {
        let (tx, mut rx) = mpsc::channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let task_alive = Arc::clone(&alive);
        let peer_listener_stop = watch::channel(false).0;
        let task = tokio::spawn(async move {
            let _liveness = EngineLivenessGuard {
                alive: task_alive,
                peer_listener_stop,
            };
            let EngineCmd::Shutdown { reply } = rx.recv().await.unwrap() else {
                return;
            };
            let _reply = reply;
            future::pending::<()>().await;
        });
        let handle = EngineHandle {
            tx,
            alive: Arc::clone(&alive),
            task: Arc::new(EngineTaskControl {
                abort: Some(task.abort_handle()),
                shutdown_timeout: Duration::from_millis(10),
                peer_listener_healthy: None,
            }),
        };

        handle.shutdown().await;
        let result = task.await;
        assert!(matches!(result, Err(error) if error.is_cancelled()));
        assert!(!handle.is_alive());
    }

    #[tokio::test]
    async fn dormant_promotion_is_detached_and_coalesced() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        std::fs::create_dir_all(&config.storage.download_dir).unwrap();
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();

        let raw = raw_single_file_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            "restore.bin".to_owned(),
            config.storage.download_dir.to_string_lossy().into_owned(),
        );
        entry.total_length = 1024;
        entry.amount_left = 1024;
        entry.state = TorrentState::Paused;
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();
        rt_db::replace_torrent_files(&mut conn, &info_hash, &meta_file_rows(&info_hash, &meta))
            .unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let mut engine = Engine {
            config: Arc::new(config),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let (resume_reply, resume_result) = oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ResumeTorrent {
                    info_hash: info_hash.clone(),
                    reply: resume_reply,
                })
                .await
        );
        assert_eq!(engine.runtime.pending_torrent_promotions.len(), 1);

        // The actor can service a second command while the promotion worker
        // reads/parses the blob. A second lifecycle request joins the same
        // pending promotion instead of constructing a duplicate task.
        let (reannounce_reply, reannounce_result) = oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ReannounceTorrent {
                    info_hash: info_hash.clone(),
                    reply: reannounce_reply,
                })
                .await
        );
        assert_eq!(
            engine.runtime.pending_torrent_promotions[&info_hash].len(),
            2
        );

        let (recheck_reply, recheck_result) = oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::RecheckTorrent {
                    info_hash: info_hash.clone(),
                    reply: recheck_reply,
                })
                .await
        );
        let recheck_job_id = {
            let db = engine.db.lock().unwrap();
            rt_db::list_active_jobs(&db)
                .unwrap()
                .into_iter()
                .find(|job| job.kind == JOB_KIND_RECHECK)
                .expect("recheck job was not persisted")
                .job_id
        };
        engine
            .control_recheck_job(&recheck_job_id, JOB_STATE_PAUSED)
            .await
            .unwrap();
        assert!(recheck_result.await.unwrap().is_err());
        let recheck_state = {
            let db = engine.db.lock().unwrap();
            rt_db::get_job(&db, &recheck_job_id).unwrap().state
        };
        assert_eq!(recheck_state, JOB_STATE_PAUSED);
        assert_eq!(
            engine.runtime.pending_torrent_promotions[&info_hash].len(),
            2
        );

        let completion = tokio::time::timeout(Duration::from_secs(2), engine.cmd_rx.recv())
            .await
            .expect("promotion worker did not complete")
            .expect("engine command channel closed");
        assert!(matches!(completion, EngineCmd::PreparedTorrentTask { .. }));
        assert!(engine.handle_cmd(completion).await);
        assert!(resume_result.await.unwrap().is_ok());
        assert!(reannounce_result.await.unwrap().is_ok());
        assert!(engine.runtime.torrent_chans.contains_key(&info_hash));
        engine.stop_torrent_task(&info_hash).await;
    }

    #[tokio::test]
    async fn engine_start_owns_background_storage_supervisor_until_shutdown() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        config.db.path = temp.path().join("state.db");
        config.network.listen_port = 0;
        config.dht.enabled = false;
        config.daemon.shutdown_timeout_secs = 1;

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let handle = Engine::start(Arc::new(config), registry).await.unwrap();
        assert!(handle.is_alive());

        let stats = handle.stats().await.unwrap();
        assert_eq!(stats.storage_workers, 2);
        assert_eq!(stats.storage_jobs_capacity, 34);
        assert_eq!(stats.storage_workers_healthy, 1);

        handle.shutdown().await;
        assert!(!handle.is_alive());
    }

    #[tokio::test]
    async fn categories_survive_engine_restart_and_keep_torrent_labels_consistent() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = temp.path().join("downloads");
        config.db.path = temp.path().join("state.db");
        config.network.listen_port = 0;
        config.dht.enabled = false;

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let engine = Engine::start(Arc::new(config.clone()), Arc::clone(&registry))
            .await
            .unwrap();
        engine
            .create_category("movies".to_owned(), Some("/srv/movies".to_owned()))
            .await
            .unwrap();
        assert_eq!(
            engine.list_categories().await.unwrap(),
            vec![EngineCategory {
                name: "movies".to_owned(),
                save_path: Some("/srv/movies".to_owned()),
            }]
        );
        engine
            .create_tags(vec!["hd".to_owned(), "remux".to_owned()])
            .await
            .unwrap();
        assert_eq!(
            engine.list_tags().await.unwrap(),
            vec!["hd".to_owned(), "remux".to_owned()]
        );
        engine
            .rename_category(
                "movies".to_owned(),
                "films".to_owned(),
                Some("/srv/films".to_owned()),
            )
            .await
            .unwrap();
        engine.shutdown().await;

        let restarted_registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let restarted = Engine::start(Arc::new(config), restarted_registry)
            .await
            .unwrap();
        assert_eq!(
            restarted.list_categories().await.unwrap(),
            vec![EngineCategory {
                name: "films".to_owned(),
                save_path: Some("/srv/films".to_owned()),
            }]
        );
        assert_eq!(
            restarted.list_tags().await.unwrap(),
            vec!["hd".to_owned(), "remux".to_owned()]
        );
        restarted
            .remove_categories(vec!["films".to_owned()])
            .await
            .unwrap();
        assert!(restarted.list_categories().await.unwrap().is_empty());
        restarted.remove_tags(vec!["hd".to_owned()]).await.unwrap();
        assert_eq!(
            restarted.list_tags().await.unwrap(),
            vec!["remux".to_owned()]
        );
        restarted.shutdown().await;
    }

    #[tokio::test]
    async fn remove_torrent_queues_payload_cleanup_outside_engine_actor() {
        let temp = tempfile::tempdir().unwrap();
        let save_root = temp.path().join("downloads");
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = save_root.clone();
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();

        let content = b"payload bytes";
        let raw = raw_single_file_torrent_with_content(content);
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        std::fs::create_dir_all(&save_root).unwrap();
        std::fs::write(save_root.join("restore.bin"), content).unwrap();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            meta.name().to_owned(),
            save_root.to_string_lossy().into_owned(),
        );
        entry.total_length = content.len() as u64;
        entry.amount_left = 0;
        entry.state = TorrentState::Seeding;
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();
        persist_torrent_files(&mut conn, &info_hash, &meta).unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let db = Arc::new(Mutex::new(conn));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::clone(&db),
            cmd_rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 2),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.remove_torrent_inner(&info_hash, true).await.unwrap();
        assert!(registry.read().await.get(&info_hash).is_some());
        assert!(rt_db::get(&db.lock().unwrap(), &info_hash).is_ok());

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !save_root.join("restore.bin").exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("payload cleanup worker did not finish");

        let jobs = rt_db::list_active_jobs(&db.lock().unwrap()).unwrap();
        assert!(jobs.is_empty());
        let completion = tokio::time::timeout(Duration::from_secs(1), engine.cmd_rx.recv())
            .await
            .expect("payload cleanup completion command did not arrive")
            .expect("engine command channel closed");
        assert!(engine.handle_cmd(completion).await);
        assert!(registry.read().await.get(&info_hash).is_none());
        assert!(rt_db::get(&db.lock().unwrap(), &info_hash).is_err());
        assert!(std::fs::read(torrent_blob_path(&engine.config, &info_hash)).is_err());
    }

    #[tokio::test]
    async fn recovered_delete_job_finalizes_metadata_after_payload_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let save_root = temp.path().join("downloads");
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.storage.download_dir = save_root.clone();
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(&save_root).unwrap();

        let content = b"recovered payload";
        let raw = raw_single_file_torrent_with_content(content);
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        let payload = save_root.join("restore.bin");
        std::fs::write(&payload, content).unwrap();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            meta.name().to_owned(),
            save_root.to_string_lossy().into_owned(),
        );
        entry.total_length = content.len() as u64;
        entry.amount_left = 0;
        entry.state = TorrentState::Seeding;
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();

        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: rt_storage::PlannedStorageAction::SafeDeleteIfPresent,
                source: Some(payload.clone()),
                destination: None,
                bytes: content.len() as u64,
            }],
            rollback_steps: Vec::new(),
        };
        let job_id = "storage-plan-delete-recovered";
        let now = unix_now_i64();
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: job_id.to_owned(),
                kind: JOB_KIND_STORAGE_PLAN.to_owned(),
                state: JOB_STATE_QUEUED.to_owned(),
                dry_run: false,
                // The original implementation left this empty; recovery
                // must use the durable context below rather than trust this
                // optional projection.
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
        let mut queued_payload = storage_plan_payload("delete", &plan, &[]);
        queued_payload["context"] = serde_json::json!({
            "info_hash": info_hash,
            "save_path_cleanup": true,
        });
        rt_db::append_job_event(
            &conn,
            &rt_db::JobEventRow {
                event_id: None,
                job_id: job_id.to_owned(),
                occurred_at: now,
                kind: "storage_plan_queued".to_owned(),
                message: Some("delete storage plan queued".to_owned()),
                payload: queued_payload.to_string(),
            },
        )
        .unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(8);
        let db = Arc::new(Mutex::new(conn));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::clone(&db),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(8).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 2),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.resume_recovered_storage_jobs().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = rt_db::get_job(&db.lock().unwrap(), job_id).unwrap().state;
                if state == JOB_STATE_COMPLETED {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recovered delete worker did not finish");
        assert!(!payload.exists());
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), job_id).unwrap().state,
            JOB_STATE_COMPLETED
        );

        engine
            .finish_storage_delete(StorageDeleteCompletion {
                job_id: job_id.to_owned(),
                info_hash: info_hash.clone(),
                succeeded: true,
                terminal_state: JOB_STATE_COMPLETED.to_owned(),
                error: None,
                completed_steps: vec![0],
                quiesced: Vec::new(),
            })
            .await
            .unwrap();
        assert!(registry.read().await.get(&info_hash).is_none());
        assert!(rt_db::get(&db.lock().unwrap(), &info_hash).is_err());
        assert!(std::fs::read(torrent_blob_path(&engine.config, &info_hash)).is_err());
    }

    #[tokio::test]
    async fn failed_payload_cleanup_keeps_torrent_retryable() {
        let info_hash = "f".repeat(40);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry
            .write()
            .await
            .add(TorrentEntry::new(
                info_hash.clone(),
                "retryable".to_owned(),
                "/tmp/downloads".to_owned(),
            ))
            .unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::from([info_hash.clone()]),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let (resume_reply, resume_result) = oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::ResumeTorrent {
                    info_hash: info_hash.clone(),
                    reply: resume_reply,
                })
                .await
        );
        assert!(resume_result
            .await
            .expect("resume guard reply was dropped")
            .is_err());

        engine
            .finish_storage_delete(StorageDeleteCompletion {
                job_id: "failed-delete".to_owned(),
                info_hash: info_hash.clone(),
                succeeded: false,
                terminal_state: JOB_STATE_FAILED.to_owned(),
                error: Some("injected delete failure".to_owned()),
                completed_steps: Vec::new(),
                quiesced: Vec::new(),
            })
            .await
            .unwrap();

        assert!(registry.read().await.get(&info_hash).is_some());
        assert!(!engine.runtime.pending_torrent_deletes.contains(&info_hash));
    }

    #[tokio::test]
    async fn storage_operation_admission_rejects_active_same_torrent_job() {
        let info_hash = "a".repeat(40);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: "storage-plan-move-active".to_owned(),
                kind: JOB_KIND_STORAGE_PLAN.to_owned(),
                state: JOB_STATE_RUNNING.to_owned(),
                dry_run: false,
                affected_torrents: vec![info_hash.clone()],
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
                started_at: Some(now),
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry
            .write()
            .await
            .add(TorrentEntry::new(
                info_hash.clone(),
                "active-move".to_owned(),
                "/tmp/downloads".to_owned(),
            ))
            .unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let error = engine
            .remove_torrent_inner(&info_hash, false)
            .await
            .unwrap_err();
        assert!(error.contains("active storage job"));
        assert!(registry.read().await.get(&info_hash).is_some());
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
        raw_single_file_torrent_with_private(false)
    }

    fn raw_single_file_torrent_with_private(private: bool) -> Vec<u8> {
        let pieces = vec![7u8; 20];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"restore.bin")),
            (b"piece length", BValue::Int(16_384)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        if private {
            info_pairs.push((b"private", BValue::Int(1)));
        }
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
    fn row_conversion_saturates_unsigned_values_at_sqlite_integer_limit() {
        let meta = meta();
        let mut entry = TorrentEntry::new("01".repeat(20), meta.name.clone(), "/tmp/data".into());
        entry.added_at = u64::MAX;
        entry.completed_at = Some(u64::MAX);
        entry.stats.uploaded = u64::MAX;
        entry.stats.downloaded = u64::MAX;

        let row = row_from_entry(&entry, &TorrentMeta::V1(meta));
        assert_eq!(row.added_at, i64::MAX);
        assert_eq!(row.completed_at, Some(i64::MAX));
        assert_eq!(row.uploaded, i64::MAX);
        assert_eq!(row.downloaded, i64::MAX);
        assert_eq!(row.total_length, 20_000);
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
    fn metadata_projection_rejects_unrepresentable_file_policy_index() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        let raw = raw_single_file_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let entry = TorrentEntry::new(info_hash.clone(), meta.name().to_owned(), "/tmp".to_owned());
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();
        rt_db::replace_torrent_files(
            &mut conn,
            &info_hash,
            &[rt_db::TorrentFileRow {
                info_hash: info_hash.clone(),
                file_index: i64::from(u32::MAX) + 1,
                path: "restore.bin".to_owned(),
                length: 1024,
                offset: 0,
                priority: 1,
                wanted: true,
                completed_bytes: 0,
            }],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let db_executor = DbExecutor::direct(db);
        let error =
            load_torrent_metadata_from_sources(&config, &db_executor, &info_hash).unwrap_err();
        assert!(error
            .to_string()
            .contains("outside the engine file-index range"));
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
        assert_eq!(files[0].path, "data.bin");
        assert_eq!(files[0].length, 65_536);

        let projected = metadata_from_meta(&meta);
        assert_eq!(projected.piece_count, 4);
        assert_eq!(projected.piece_states.len(), 4);
        assert_eq!(projected.files[0].path, "data.bin");
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
        std::fs::create_dir_all(&save_root).unwrap();
        std::fs::write(save_root.join("data.bin"), &content).unwrap();
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
        let (cmd_tx, rx) = mpsc::channel(1);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };
        // Exercise the production command path: the actor should only
        // dispatch the recheck and receive a completion notification. The
        // file-root verifier itself must not run inside `handle_cmd`.
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::RecheckTorrent {
                    info_hash: info_hash.clone(),
                    reply,
                })
                .await
        );
        reply_rx.await.unwrap().unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(2), engine.cmd_rx.recv())
            .await
            .expect("pure v2 recheck worker did not send completion")
            .expect("engine command channel closed");
        let job_id = match &completion {
            EngineCmd::PureV2RecheckFinished {
                job_id: Some(job_id),
                ..
            } => job_id.clone(),
            other => panic!("unexpected pure v2 completion: {other:?}"),
        };
        assert!(engine.handle_cmd(completion).await);

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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
        assert!(engine.runtime.torrent_chans.is_empty());
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
        assert!(engine.runtime.torrent_chans.is_empty());
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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

        assert!(engine.runtime.torrent_chans.is_empty());
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
        assert!(engine.runtime.torrent_chans.is_empty());

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

    #[tokio::test]
    async fn private_magnet_completion_removes_provisional_dht_and_preserves_pause() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        config.daemon.session_dir = temp.path().join("session");
        std::fs::create_dir_all(&config.storage.download_dir).unwrap();
        std::fs::create_dir_all(&config.daemon.session_dir).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (dht_tx, mut dht_rx) = mpsc::channel(8);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: Some(dht_tx),
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let raw = raw_single_file_torrent_with_private(true);
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        let info_hash_bytes = match meta {
            TorrentMeta::V1(meta) => meta.info_hash,
            _ => unreachable!("private fixture must be a v1 torrent"),
        };
        let magnet = MagnetLink {
            info_hash_v1: Some(info_hash_bytes),
            info_hash_v2: None,
            display_name: Some("private.bin".to_owned()),
            trackers: vec!["http://tracker.example.com/announce".to_owned()],
        };

        // A v1 magnet has to use DHT while metadata is pending because its
        // private flag is not known yet. Abort the metadata worker immediately
        // so this test observes only the lifecycle messages under test.
        engine
            .add_magnet(
                magnet,
                Some(engine.config.storage.download_dir.clone()),
                false,
                None,
                Vec::new(),
            )
            .await
            .unwrap();
        if let Some(task) = engine.runtime.torrent_tasks.get(&info_hash) {
            task.abort();
        }
        assert!(matches!(
            dht_rx.recv().await,
            Some(DhtCommand::AddTorrent(torrent)) if torrent.info_hash == info_hash_bytes
        ));

        // Simulate the user pausing the metadata-pending torrent after the
        // provisional DHT registration. The public pause path sends this
        // same unregister operation before persisting the state; keeping the
        // setup direct avoids making a second worker failure part of the test.
        engine.unregister_dht_torrent(&info_hash).await;
        assert!(matches!(
            dht_rx.recv().await,
            Some(DhtCommand::RemoveTorrent(hash)) if hash == info_hash_bytes
        ));
        engine
            .update_metadata_placeholder_state_with_event(&info_hash, TorrentState::Paused, None)
            .await
            .unwrap();

        engine.complete_magnet(&info_hash, raw).await.unwrap();
        assert!(matches!(
            dht_rx.recv().await,
            Some(DhtCommand::RemoveTorrent(hash)) if hash == info_hash_bytes
        ));
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Paused
        );

        engine.shutdown_torrent_tasks().await;
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        persist_test_torrent(&engine, &"b".repeat(40));
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
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        persist_test_torrent(&engine, &info_hash);
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
    async fn pause_torrent_pauses_taskless_pure_v2_recheck_job() {
        let temp = tempfile::tempdir().unwrap();
        let raw = raw_v2_torrent();
        let meta = parse_torrent(&raw).unwrap();
        let info_hash = meta_info_hash_hex(&meta);
        let mut entry = rt_session::TorrentEntry::new(
            info_hash.clone(),
            meta.name().to_owned(),
            temp.path().to_string_lossy().into_owned(),
        );
        entry.total_length = meta_total_length(&meta);
        entry.amount_left = entry.total_length;
        entry.state = TorrentState::Paused;
        let mut session_registry = SessionRegistry::new();
        session_registry.add(entry.clone()).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        rt_db::upsert(&conn, &row_from_entry(&entry, &meta)).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(session_registry));
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let job_id = engine.create_recheck_job(&info_hash).unwrap();
        let (reply, rx) = tokio::sync::oneshot::channel();
        assert!(
            engine
                .handle_cmd(EngineCmd::PauseTorrent {
                    info_hash: info_hash.clone(),
                    reply,
                })
                .await
        );
        rx.await.unwrap().unwrap();
        assert_eq!(
            registry.read().await.get(&info_hash).unwrap().state,
            TorrentState::Paused
        );
        let db = engine.db.lock().unwrap();
        assert_eq!(
            rt_db::get_job(&db, &job_id).unwrap().state,
            JOB_STATE_PAUSED
        );
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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

    #[tokio::test]
    async fn global_limits_persist_to_settings_table() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        assert_eq!(
            engine.global_limits_inner().await.unwrap(),
            EngineGlobalLimits::default()
        );
        engine
            .update_global_limits_inner(EngineGlobalLimits {
                download_limit: 123,
                upload_limit: 456,
                speed_limits_mode: true,
            })
            .await
            .unwrap();
        assert_eq!(
            engine.global_limits_inner().await.unwrap(),
            EngineGlobalLimits {
                download_limit: 123,
                upload_limit: 456,
                speed_limits_mode: true,
            }
        );
    }

    #[tokio::test]
    async fn malformed_persisted_control_settings_fail_closed() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        {
            let db = engine.db.lock().unwrap();
            rt_db::set_setting(&db, SETTING_GLOBAL_DOWNLOAD_LIMIT, "not-a-rate", 1).unwrap();
            rt_db::set_setting(&db, SETTING_GLOBAL_SPEED_LIMITS_MODE, "maybe", 1).unwrap();
            rt_db::set_setting(&db, SETTING_NETWORK_DHT, "maybe", 1).unwrap();
        }
        assert!(engine.global_limits_inner().await.is_err());
        assert!(engine.network_features_inner().is_err());
    }

    #[tokio::test]
    async fn user_agent_update_persists_and_changes_runtime_clients() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine
            .set_user_agent_inner("TorrentNG/test".to_owned())
            .await
            .unwrap();
        let db = engine.db.lock().unwrap();
        assert_eq!(
            rt_db::get_setting(&db, SETTING_NETWORK_USER_AGENT).unwrap(),
            "TorrentNG/test"
        );
        drop(db);
        assert_eq!(crate::peer_id::user_agent(), "TorrentNG/test");
        crate::peer_id::set_user_agent(crate::peer_id::DEFAULT_USER_AGENT.to_owned()).unwrap();
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let update = engine.update_torrent_limits_inner(
            &info_hash,
            EngineTorrentLimits {
                sequential_download: true,
                sequential_download_from_piece: Some(5),
                ..EngineTorrentLimits::default()
            },
        );
        tokio::pin!(update);
        let command = tokio::select! {
            result = &mut update => panic!("update completed before runtime acknowledgement: {result:?}"),
            command = torrent_rx.recv() => command,
        };
        match command {
            Some(TorrentCmd::UpdateLimits {
                limits,
                reply: Some(reply),
            }) => {
                assert!(limits.sequential_download);
                assert_eq!(limits.sequential_download_from_piece, Some(5));
                reply.send(Ok(())).unwrap();
            }
            other => panic!("expected runtime limit update, got {other:?}"),
        }
        assert!(update.await.is_ok());
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks,
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.shutdown_torrent_tasks().await;

        assert!(seen_rx.await.is_ok());
        assert!(engine.runtime.torrent_chans.is_empty());
        assert!(engine.runtime.torrent_tasks.is_empty());
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            recovered_storage_plan_steps(&plan, 1, Some(vec![1])).unwrap(),
            vec![1],
            "restart must preserve sparse completed-step indexes"
        );
        assert_eq!(
            recovered_storage_plan_steps(&plan, 2, None).unwrap(),
            vec![0, 1],
            "legacy count fallback remains a prefix only when the event is absent"
        );
        assert_eq!(
            recovered_storage_plan_steps(&plan, 2, Some(Vec::new())).unwrap(),
            Vec::<usize>::new(),
            "an explicit empty event must not be replaced by a stale checkpoint prefix"
        );
        assert!(recovered_storage_plan_steps(&plan, 0, Some(vec![2])).is_err());
        assert!(recovered_storage_plan_steps(&plan, -1, None).is_err());
    }

    #[tokio::test]
    async fn recovered_storage_plan_reconciles_filesystem_ahead_of_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("old");
        let destination = root.join("destination");
        std::fs::write(&destination, b"payload").unwrap();

        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        config.storage.download_dir = root.clone();
        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();

        let info_hash = "a".repeat(40);
        let old_save_path = root.join("old");
        let now = unix_now_i64();
        rt_db::upsert(
            &conn,
            &rt_db::TorrentRow {
                info_hash: info_hash.clone(),
                name: "payload".to_owned(),
                total_length: 7,
                piece_length: 7,
                piece_count: 1,
                is_private: false,
                save_path: old_save_path.to_string_lossy().into_owned(),
                category: None,
                tags: Vec::new(),
                state: TorrentState::Paused.as_str().to_owned(),
                added_at: now,
                completed_at: None,
                uploaded: 0,
                downloaded: 7,
                ratio: 0.0,
                trackers: Vec::new(),
            },
        )
        .unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            "payload".to_owned(),
            old_save_path.to_string_lossy().into_owned(),
        );
        entry.state = TorrentState::Paused;
        entry.total_length = 7;
        entry.amount_left = 0;
        registry.write().await.add(entry).unwrap();

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
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: job_id.to_owned(),
                kind: JOB_KIND_STORAGE_PLAN.to_owned(),
                state: JOB_STATE_QUEUED.to_owned(),
                dry_run: false,
                affected_torrents: vec![info_hash.clone()],
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
                payload: {
                    let mut payload = storage_plan_payload("move", &plan, &[]);
                    payload["context"] = serde_json::json!({
                        "old_save_path": old_save_path,
                        "save_path": root.join("destination"),
                        "name": "payload",
                    });
                    payload.to_string()
                },
            },
        )
        .unwrap();

        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let db = Arc::new(Mutex::new(conn));
        let storage_jobs = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 2);
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db,
            cmd_rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs,
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.resume_recovered_storage_jobs().await.unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(2), engine.cmd_rx.recv())
            .await
            .expect("recovered storage completion was not delivered")
            .expect("engine command channel closed");
        assert!(engine.handle_cmd(completion).await);

        {
            let db = engine.db.lock().unwrap();
            let job = rt_db::get_job(&db, job_id).unwrap();
            assert_eq!(job.done, 1);
            assert_eq!(job.checkpoint, 1);
            assert_eq!(std::fs::read(destination).unwrap(), b"payload");
            assert!(rt_db::list_active_jobs(&db).unwrap().is_empty());
        }
        let registry = registry.read().await;
        assert_eq!(
            registry.get(&info_hash).unwrap().save_path,
            root.join("destination").to_string_lossy()
        );
    }

    #[tokio::test]
    async fn storage_move_db_failure_keeps_destination_live_and_commit_pending() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        config.storage.download_dir = root.clone();
        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();

        let info_hash = "b".repeat(40);
        let old_save_path = root.join("old");
        let save_path = root.join("new");
        let now = unix_now_i64();
        rt_db::upsert(
            &conn,
            &rt_db::TorrentRow {
                info_hash: info_hash.clone(),
                name: "payload".to_owned(),
                total_length: 7,
                piece_length: 7,
                piece_count: 1,
                is_private: false,
                save_path: old_save_path.to_string_lossy().into_owned(),
                category: None,
                tags: Vec::new(),
                state: TorrentState::Paused.as_str().to_owned(),
                added_at: now,
                completed_at: None,
                uploaded: 0,
                downloaded: 7,
                ratio: 0.0,
                trackers: Vec::new(),
            },
        )
        .unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            "payload".to_owned(),
            old_save_path.to_string_lossy().into_owned(),
        );
        entry.state = TorrentState::Paused;
        registry.write().await.add(entry).unwrap();

        let job_id = "storage-move-db-failure";
        rt_db::upsert_job(
            &conn,
            &rt_db::JobRow {
                job_id: job_id.to_owned(),
                kind: JOB_KIND_STORAGE_PLAN.to_owned(),
                state: STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                dry_run: false,
                affected_torrents: vec![info_hash.clone()],
                total: 1,
                done: 1,
                checkpoint: 1,
                file_index: Some(1),
                piece_index: None,
                byte_offset: Some(7),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: Some(now),
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();

        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let db = Arc::new(Mutex::new(conn));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::clone(&db),
            cmd_rx,
            cmd_tx,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        db.lock()
            .unwrap()
            .execute_batch("PRAGMA query_only = ON")
            .unwrap();
        let result = engine
            .finish_storage_move(
                job_id,
                &info_hash,
                None,
                old_save_path.clone(),
                save_path.clone(),
                None,
                true,
                STORAGE_JOB_STATE_COMMIT_PENDING.to_owned(),
                None,
                vec![0],
                0,
            )
            .await;
        assert!(result.is_err());

        let registry = registry.read().await;
        assert_eq!(
            registry.get(&info_hash).unwrap().save_path,
            save_path.to_string_lossy()
        );
        drop(registry);
        let db = db.lock().unwrap();
        assert_eq!(
            rt_db::get(&db, &info_hash).unwrap().save_path,
            old_save_path.to_string_lossy()
        );
        assert_eq!(
            rt_db::get_job(&db, job_id).unwrap().state,
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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

        let mut conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let persisted_meta = TorrentMeta::V1(meta.clone());
        let mut entry = TorrentEntry::new(
            info_hash.clone(),
            meta.name.clone(),
            source_root.to_string_lossy().into(),
        );
        entry.total_length = meta.total_length();
        entry.amount_left = 0;
        entry.state = TorrentState::Paused;
        rt_db::upsert(&conn, &row_from_entry(&entry, &persisted_meta)).unwrap();
        persist_torrent_files(&mut conn, &info_hash, &persisted_meta).unwrap();
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs,
                stats_cache: None,
            },
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

        let mut conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        let persisted_meta = TorrentMeta::V1(meta.clone());
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
        persist_torrent_files(&mut conn, &info_hash, &persisted_meta).unwrap();
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs,
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();
        assert!(
            engine.runtime.torrent_chans.contains_key(&info_hash),
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

        if let Some(tx) = engine.runtime.torrent_chans.remove(&info_hash) {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
    }

    #[tokio::test]
    async fn startup_reconciles_missing_rows_and_quarantines_orphan_projections() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let missing_hash = "a".repeat(40);
        let orphan_hash = "b".repeat(40);
        std::fs::write(
            torrent_blob_dir(&config).join(format!("{orphan_hash}.torrent")),
            b"orphan metainfo",
        )
        .unwrap();
        std::fs::write(
            fastresume_dir(&config).join(format!("{orphan_hash}.fastresume.json")),
            b"orphan resume",
        )
        .unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        register_configured_storage(&conn, &config).unwrap();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: missing_hash.clone(),
                name: "missing-metainfo".to_owned(),
                total_length: 1024,
                piece_length: 1024,
                piece_count: 1,
                is_private: false,
                save_path: config.storage.download_dir.to_string_lossy().into_owned(),
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

        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config.clone()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        {
            let registry = registry.read().await;
            assert_eq!(
                registry.get(&missing_hash).unwrap().state,
                TorrentState::Error
            );
            assert_eq!(registry.active_len(), 0);
            assert_eq!(registry.dormant_len(), 1);
        }
        {
            let db = engine.db.lock().unwrap();
            assert_eq!(rt_db::get(&db, &missing_hash).unwrap().state, "error");
            let issues = rt_db::list_active_issues(&db).unwrap();
            assert!(issues.iter().any(|issue| {
                issue.info_hash.as_deref() == Some(&missing_hash)
                    && issue.artifact == "torrent_blob"
            }));
            assert!(issues.iter().any(|issue| {
                issue.info_hash.as_deref() == Some(&orphan_hash) && issue.artifact == "torrent_blob"
            }));
            assert!(issues.iter().any(|issue| {
                issue.info_hash.as_deref() == Some(&orphan_hash) && issue.artifact == "fastresume"
            }));
        }
        assert!(!torrent_blob_dir(&config)
            .join(format!("{orphan_hash}.torrent"))
            .exists());
        assert!(!fastresume_dir(&config)
            .join(format!("{orphan_hash}.fastresume.json"))
            .exists());
        assert!(torrent_blob_dir(&config)
            .join("quarantine")
            .join(format!("{orphan_hash}.torrent"))
            .is_file());
        assert!(fastresume_dir(&config)
            .join("quarantine")
            .join(format!("{orphan_hash}.fastresume.json"))
            .is_file());
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
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(
            !engine.runtime.torrent_chans.contains_key(&info_hash),
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
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(
            !engine.runtime.torrent_chans.contains_key(&info_hash),
            "idle seed restore should retain only the dormant representation"
        );
        let reg = registry.read().await;
        let restored = reg.get(&info_hash).unwrap();
        assert_eq!(restored.state, TorrentState::Seeding);
        drop(reg);
        assert!(
            engine
                .runtime
                .tier_controller
                .next_tracker_deadline()
                .is_some(),
            "dormant seeding restore must schedule its persisted tracker deadline"
        );
        assert!(
            engine
                .runtime
                .tier_controller
                .dormant_snapshot(&info_hash)
                .is_some(),
            "dormant seeding restore must retain the compact runtime projection"
        );
        assert!(engine.runtime.tier_controller.dormant_heap_bytes() > 0);
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(engine.runtime.torrent_chans.is_empty());
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: Some(dht_tx),
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };
        persist_test_torrent(&engine, &info_hash);
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
                    tracker_id: None,
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: Some(dht_tx),
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        let health = engine.engine_subsystem_health().await.unwrap();
        assert!(health.dht_enabled);
        assert!(!health.dht_healthy);
        assert!(!health.storage_workers_healthy);
        assert!(!engine.network_features_inner().unwrap().dht);
    }

    #[tokio::test]
    async fn finished_torrent_task_is_removed_and_marked_error() {
        let info_hash = "e".repeat(40);
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: "failed-task".to_owned(),
                total_length: 1,
                piece_length: 1,
                piece_count: 1,
                is_private: false,
                save_path: "/tmp".to_owned(),
                category: None,
                tags: Vec::new(),
                state: "downloading".to_owned(),
                added_at: 1,
                completed_at: None,
                uploaded: 0,
                downloaded: 0,
                ratio: 0.0,
                trackers: Vec::new(),
            },
        )
        .unwrap();
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut entry = TorrentEntry::new(info_hash.clone(), "failed-task".into(), "/tmp".into());
        entry.transition(TorrentState::Downloading).unwrap();
        registry.write().await.add(entry).unwrap();

        let (_cmd_tx, cmd_rx) = mpsc::channel(1);
        let (torrent_tx, _torrent_rx) = mpsc::channel(1);
        let failed_task = tokio::spawn(async {
            panic!("injected torrent task failure");
        });
        tokio::task::yield_now().await;
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let mut torrent_tasks = HashMap::new();
        torrent_tasks.insert(info_hash.clone(), failed_task);
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx,
            cmd_tx: mpsc::channel(1).0,
            runtime: subsystems::EngineRuntimeState {
                torrent_chans,
                torrent_tasks,
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
            shutdown_reply: None,
        };

        engine.reap_finished_torrent_tasks().await;

        assert!(!engine.runtime.torrent_chans.contains_key(&info_hash));
        assert!(!engine.runtime.torrent_tasks.contains_key(&info_hash));
        let registry = registry.read().await;
        let failed = registry.get(&info_hash).unwrap();
        assert_eq!(failed.state, TorrentState::Error);
        assert_eq!(registry.active_len(), 0);
        assert_eq!(registry.dormant_len(), 1);
        assert!(failed
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("panicked")));
        let db = engine.db.lock().unwrap();
        assert_eq!(rt_db::get(&db, &info_hash).unwrap().state, "error");
        let events = rt_db::list_session_events(&db, Some(&info_hash), 10).unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == "torrent_task_failed"));
        assert_eq!(
            engine.runtime.tier_controller.tier(&info_hash),
            Some(TorrentActivityTier::Dormant)
        );
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: tiny_api_snapshot_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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
            runtime: subsystems::EngineRuntimeState {
                torrent_chans: HashMap::new(),
                torrent_tasks: HashMap::new(),
                tier_controller: TierController::new(TierPolicy::default()),
                tier_last_active: HashMap::new(),
                pending_torrent_adds: HashSet::new(),
                pending_torrent_deletes: HashSet::new(),
                pending_torrent_promotions: std::collections::HashMap::new(),
            },
            services: subsystems::EngineSubsystems {
                dht_tx: None,
                resources: test_resource_governor(),
                network_budget: GlobalNetworkBudget::unlimited(),
                storage_jobs: StorageJobDispatcher::for_tests(),
                stats_cache: None,
            },
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

fn torrent_command_peer_addr(command: &TorrentCmd) -> Option<SocketAddr> {
    match command {
        TorrentCmd::AcceptPeer { peer_addr, .. } | TorrentCmd::AcceptUtpPeer { peer_addr, .. } => {
            Some(*peer_addr)
        }
        _ => None,
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
