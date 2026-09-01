use rt_api_model::TorrentSummary;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

use rt_engine::EngineHandle;
use rt_session::SessionRegistry;

pub type JsonMap = BTreeMap<String, serde_json::Value>;

const TORRENT_SNAPSHOT_CACHE_SIZE: usize = 4;
const TORRENT_SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone)]
pub struct TorrentSnapshot {
    pub revision: u64,
    pub torrents: Arc<Vec<TorrentSnapshotItem>>,
}

#[derive(Debug, Clone)]
pub struct TorrentSnapshotItem {
    pub summary: TorrentSummary,
    pub amount_left: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentSnapshotError {
    Expired { revision: u64 },
}

#[derive(Clone)]
struct CachedTorrentSnapshot {
    snapshot: TorrentSnapshot,
    generated_at: Instant,
}

/// Shared application state threaded through axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<Vec<String>>>,
    pub saved_views: Arc<RwLock<JsonMap>>,
    pub ratio_groups: Arc<RwLock<JsonMap>>,
    pub workflows: Arc<RwLock<JsonMap>>,
    pub workflow_runs: Arc<RwLock<Vec<serde_json::Value>>>,
    pub rss_rules: Arc<RwLock<JsonMap>>,
    pub user_agent: Arc<RwLock<String>>,
    torrent_snapshot_cache: Arc<RwLock<VecDeque<CachedTorrentSnapshot>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::from_parts(
            Arc::new(RwLock::new(SessionRegistry::new())),
            None,
            Vec::new(),
        )
    }

    fn from_parts(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: Option<EngineHandle>,
        api_tokens: Vec<String>,
    ) -> Self {
        AppState {
            registry,
            engine,
            api_tokens: Arc::new(api_tokens),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent().to_owned())),
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self::from_parts(registry, None, Vec::new())
    }

    pub fn with_tokens(engine: Option<EngineHandle>, api_tokens: Vec<String>) -> Self {
        Self::from_parts(
            Arc::new(RwLock::new(SessionRegistry::new())),
            engine,
            api_tokens,
        )
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self::from_parts(registry, Some(engine), Vec::new())
    }

    pub fn with_engine_and_tokens(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
    ) -> Self {
        Self::from_parts(registry, Some(engine), api_tokens)
    }

    /// Return a bounded, immutable summary snapshot for pagination and other
    /// read-heavy consumers.  A request without a cursor may reuse a recent
    /// snapshot; a request with a cursor is pinned to that exact generation.
    pub async fn torrent_snapshot(
        &self,
        requested_revision: Option<u64>,
    ) -> Result<TorrentSnapshot, TorrentSnapshotError> {
        {
            let cache = self.torrent_snapshot_cache.read().await;
            if let Some(revision) = requested_revision {
                if let Some(cached) = cache
                    .iter()
                    .find(|cached| cached.snapshot.revision == revision)
                {
                    return Ok(cached.snapshot.clone());
                }
                return Err(TorrentSnapshotError::Expired { revision });
            }

            let current_revision = self.registry.read().await.revision();
            if let Some(cached) = cache.front().filter(|cached| {
                cached.snapshot.revision == current_revision
                    || cached.generated_at.elapsed() <= TORRENT_SNAPSHOT_MAX_AGE
            }) {
                return Ok(cached.snapshot.clone());
            }
        }

        let (revision, torrents) = {
            let registry = self.registry.read().await;
            let revision = registry.revision();
            let torrents = registry
                .iter()
                .map(|entry| TorrentSnapshotItem {
                    summary: torrent_summary(entry),
                    amount_left: entry.amount_left,
                })
                .collect::<Vec<_>>();
            (revision, Arc::new(torrents))
        };
        let snapshot = TorrentSnapshot { revision, torrents };
        let mut cache = self.torrent_snapshot_cache.write().await;
        cache.retain(|cached| cached.snapshot.revision != revision);
        cache.push_front(CachedTorrentSnapshot {
            snapshot: snapshot.clone(),
            generated_at: Instant::now(),
        });
        cache.truncate(TORRENT_SNAPSHOT_CACHE_SIZE);
        Ok(snapshot)
    }
}

pub(crate) fn torrent_summary(entry: &rt_session::TorrentEntry) -> TorrentSummary {
    TorrentSummary {
        info_hash: entry.info_hash.clone(),
        name: entry.name.clone(),
        state: entry.state.as_str().to_owned(),
        total_length: entry.total_length as i64,
        downloaded: entry.stats.downloaded as i64,
        uploaded: entry.stats.uploaded as i64,
        ratio: entry.stats.ratio(),
        save_path: entry.save_path.clone(),
        category: entry.category.clone(),
        tags: entry.tags.clone(),
        added_at: entry.added_at as i64,
        completed_at: entry.completed_at.map(|value| value as i64),
        num_peers: 0,
        num_seeds: 0,
        tracker_message: entry.tracker_message.clone(),
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
