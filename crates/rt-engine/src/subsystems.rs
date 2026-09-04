//! Explicit ownership boundaries for the engine actor.
//!
//! `Engine` is still the single ordering authority, but it should not also
//! present every runtime concern as a flat bag of fields. These two state
//! objects make the ownership boundary visible:
//!
//! * `EngineRuntimeState` owns torrent actors, tier admission, and lifecycle
//!   bookkeeping that is mutated by the actor.
//! * `EngineSubsystems` owns detachable services and process-wide budgets.
//!
//! Keeping these objects free of cross-layer callbacks makes the next step—a
//! separately supervised actor or injected dependency—mechanical rather than
//! another rewrite of the command protocol.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::command::{EngineStats, TorrentPromotionAction};
use crate::dht_task::DhtCommand;
use crate::network_budget::GlobalNetworkBudget;
use crate::storage_jobs::StorageJobDispatcher;
use crate::tier::TierController;
use crate::torrent_task::TorrentCmd;
use rt_metrics::{ResourceGovernor, ResourceGovernorConfig};

pub(super) struct EngineRuntimeState {
    pub(super) torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    pub(super) torrent_tasks: HashMap<String, JoinHandle<()>>,
    pub(super) tier_controller: TierController<String>,
    pub(super) tier_last_active: HashMap<String, Instant>,
    pub(super) pending_torrent_adds: HashSet<String>,
    pub(super) pending_torrent_deletes: HashSet<String>,
    pub(super) pending_torrent_promotions: HashMap<String, Vec<TorrentPromotionAction>>,
}

impl EngineRuntimeState {
    pub(super) fn new(tier_controller: TierController<String>) -> Self {
        Self {
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            tier_controller,
            tier_last_active: HashMap::new(),
            pending_torrent_adds: HashSet::new(),
            pending_torrent_deletes: HashSet::new(),
            pending_torrent_promotions: HashMap::new(),
        }
    }
}

pub(super) struct EngineSubsystems {
    pub(super) dht_tx: Option<mpsc::Sender<DhtCommand>>,
    pub(super) resources: ResourceGovernor,
    pub(super) network_budget: GlobalNetworkBudget,
    pub(super) storage_jobs: StorageJobDispatcher,
    pub(super) stats_cache: Option<EngineStatsCache>,
}

pub(super) struct EngineStatsCache {
    pub(super) generated_at: Instant,
    pub(super) stats: EngineStats,
    /// The refresh is detached from the actor. Keep its start time so a
    /// dropped completion cannot permanently suppress future refreshes.
    pub(super) refresh_started_at: Option<Instant>,
}

impl EngineSubsystems {
    pub(super) fn new(
        dht_tx: Option<mpsc::Sender<DhtCommand>>,
        resource_config: ResourceGovernorConfig,
        network_budget: GlobalNetworkBudget,
        storage_jobs: StorageJobDispatcher,
    ) -> Self {
        Self {
            dht_tx,
            resources: ResourceGovernor::new(resource_config),
            network_budget,
            storage_jobs,
            stats_cache: None,
        }
    }
}
