use rt_api_model::{ApiRuntimeMetrics, TorrentSummary};
use std::sync::Mutex as StdMutex;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};

use rt_engine::EngineHandle;
use rt_session::SessionRegistry;

pub type JsonMap = BTreeMap<String, serde_json::Value>;

const TORRENT_SNAPSHOT_CACHE_SIZE: usize = 4;
const TORRENT_SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(750);
type TorrentOrderCache = StdMutex<HashMap<&'static str, Arc<Vec<usize>>>>;

#[derive(Debug, Clone)]
pub struct TorrentSnapshot {
    pub revision: u64,
    pub torrents: Arc<Vec<TorrentSnapshotItem>>,
    orders: Arc<TorrentOrderCache>,
    filters: Arc<TorrentFilterIndex>,
}

#[derive(Debug, Clone)]
pub struct TorrentSnapshotItem {
    pub summary: TorrentSummary,
    pub amount_left: u64,
    name_sort_key: String,
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

#[derive(Debug, Default)]
struct TorrentFilterIndex {
    by_state: HashMap<String, Arc<Vec<usize>>>,
    by_category: HashMap<String, Arc<Vec<usize>>>,
    by_tag: HashMap<String, Arc<Vec<usize>>>,
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
    pub(crate) api_metrics: Arc<ApiRuntimeMetrics>,
    torrent_snapshot_cache: Arc<RwLock<VecDeque<CachedTorrentSnapshot>>>,
    torrent_snapshot_refresh: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::from_parts(
            Arc::new(RwLock::new(SessionRegistry::new())),
            None,
            Vec::new(),
            ApiRuntimeMetrics::new(),
        )
    }

    fn from_parts(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: Option<EngineHandle>,
        api_tokens: Vec<String>,
        api_metrics: Arc<ApiRuntimeMetrics>,
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
            api_metrics,
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
            torrent_snapshot_refresh: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self::from_parts(registry, None, Vec::new(), ApiRuntimeMetrics::new())
    }

    pub fn with_tokens(engine: Option<EngineHandle>, api_tokens: Vec<String>) -> Self {
        Self::from_parts(
            Arc::new(RwLock::new(SessionRegistry::new())),
            engine,
            api_tokens,
            ApiRuntimeMetrics::new(),
        )
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self::from_parts(registry, Some(engine), Vec::new(), ApiRuntimeMetrics::new())
    }

    pub fn with_engine_and_tokens(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
    ) -> Self {
        Self::from_parts(registry, Some(engine), api_tokens, ApiRuntimeMetrics::new())
    }

    pub fn with_engine_and_tokens_and_metrics(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
        api_metrics: Arc<ApiRuntimeMetrics>,
    ) -> Self {
        Self::from_parts(registry, Some(engine), api_tokens, api_metrics)
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
                self.api_metrics.record_snapshot_expired();
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

        // Only one request may refresh the shared snapshot. Recheck after
        // taking the lock so concurrent list/SSE clients do not all clone the
        // registry after observing the same expired cache entry.
        let _refresh = self.torrent_snapshot_refresh.lock().await;
        {
            let cache = self.torrent_snapshot_cache.read().await;
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
                    name_sort_key: entry.name.to_ascii_lowercase(),
                })
                .collect::<Vec<_>>();
            (revision, Arc::new(torrents))
        };
        let filters = Arc::new(build_filter_index(&torrents));
        self.api_metrics.record_snapshot_refresh();
        let snapshot = TorrentSnapshot {
            revision,
            torrents,
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters,
        };
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

impl TorrentSnapshot {
    /// Return candidate indexes for exact operational filters. The returned
    /// indexes are sorted in snapshot order, so callers can intersect them
    /// and use binary search while traversing a requested sort order.
    pub(crate) fn candidate_indices(
        &self,
        states: &[&str],
        category: Option<&str>,
        tag: Option<&str>,
    ) -> Option<Vec<usize>> {
        let mut candidates = if states.is_empty() {
            None
        } else {
            let mut union = Vec::new();
            for state in states {
                if let Some(indexes) = self.filters.by_state.get(*state) {
                    union.extend(indexes.iter().copied());
                }
            }
            union.sort_unstable();
            union.dedup();
            Some(union)
        };

        for indexes in [
            category.and_then(|value| self.filters.by_category.get(value)),
            tag.and_then(|value| self.filters.by_tag.get(value)),
        ]
        .into_iter()
        {
            let Some(indexes) = indexes else {
                if category.is_some() || tag.is_some() {
                    // Only treat the relevant missing index as an empty
                    // result; an omitted filter must not erase candidates.
                    continue;
                }
                continue;
            };
            candidates = Some(match candidates {
                Some(current) => intersect_sorted(&current, indexes),
                None => indexes.as_ref().clone(),
            });
        }
        if category.is_some_and(|value| !self.filters.by_category.contains_key(value))
            || tag.is_some_and(|value| !self.filters.by_tag.contains_key(value))
        {
            return Some(Vec::new());
        }
        candidates
    }
}

fn build_filter_index(torrents: &[TorrentSnapshotItem]) -> TorrentFilterIndex {
    let mut by_state = HashMap::<String, Vec<usize>>::new();
    let mut by_category = HashMap::<String, Vec<usize>>::new();
    let mut by_tag = HashMap::<String, Vec<usize>>::new();
    for (index, item) in torrents.iter().enumerate() {
        by_state
            .entry(item.summary.state.clone())
            .or_default()
            .push(index);
        if let Some(category) = &item.summary.category {
            by_category.entry(category.clone()).or_default().push(index);
        }
        let mut seen_tags = HashSet::new();
        for tag in &item.summary.tags {
            if seen_tags.insert(tag) {
                by_tag.entry(tag.clone()).or_default().push(index);
            }
        }
    }
    TorrentFilterIndex {
        by_state: by_state
            .into_iter()
            .map(|(key, indexes)| (key, Arc::new(indexes)))
            .collect(),
        by_category: by_category
            .into_iter()
            .map(|(key, indexes)| (key, Arc::new(indexes)))
            .collect(),
        by_tag: by_tag
            .into_iter()
            .map(|(key, indexes)| (key, Arc::new(indexes)))
            .collect(),
    }
}

fn intersect_sorted(left: &[usize], right: &[usize]) -> Vec<usize> {
    let mut result = Vec::with_capacity(left.len().min(right.len()));
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            Ordering::Less => left_index += 1,
            Ordering::Greater => right_index += 1,
            Ordering::Equal => {
                result.push(left[left_index]);
                left_index += 1;
                right_index += 1;
            }
        }
    }
    result
}

impl TorrentSnapshot {
    /// Return an immutable ascending index for a supported sort. The index is
    /// built once per cached snapshot and reused by every page request that
    /// references that snapshot; reverse order is handled by the caller.
    pub(crate) fn ordered_indices(&self, requested_sort: Option<&str>) -> Arc<Vec<usize>> {
        let sort = match requested_sort.unwrap_or("added") {
            "name" => "name",
            "size" | "total_length" => "size",
            "ratio" => "ratio",
            "progress" => "progress",
            "hash" => "hash",
            _ => "added",
        };
        if let Some(indices) = self
            .orders
            .lock()
            .expect("torrent snapshot order mutex poisoned")
            .get(sort)
            .cloned()
        {
            return indices;
        }

        let mut indices = (0..self.torrents.len()).collect::<Vec<_>>();
        indices.sort_unstable_by(|left, right| {
            let left = &self.torrents[*left];
            let right = &self.torrents[*right];
            let ordering = match sort {
                "name" => left.name_sort_key.cmp(&right.name_sort_key),
                "size" => left.summary.total_length.cmp(&right.summary.total_length),
                "ratio" => left
                    .summary
                    .ratio
                    .partial_cmp(&right.summary.ratio)
                    .unwrap_or(Ordering::Equal),
                "progress" => progress(left).cmp(&progress(right)),
                "hash" => left.summary.info_hash.cmp(&right.summary.info_hash),
                _ => left.summary.added_at.cmp(&right.summary.added_at),
            };
            ordering.then_with(|| left.summary.info_hash.cmp(&right.summary.info_hash))
        });
        let indices = Arc::new(indices);
        self.orders
            .lock()
            .expect("torrent snapshot order mutex poisoned")
            .insert(sort, Arc::clone(&indices));
        indices
    }
}

fn progress(item: &TorrentSnapshotItem) -> u64 {
    item.summary
        .total_length
        .max(0)
        .try_into()
        .unwrap_or(u64::MAX)
        .saturating_sub(item.amount_left)
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
