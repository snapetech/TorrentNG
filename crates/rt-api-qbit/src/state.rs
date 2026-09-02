use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};

use rt_engine::{EngineGlobalLimits, EngineHandle};
use rt_session::{SessionRegistry, TorrentEntry};

pub type JsonMap = serde_json::Map<String, serde_json::Value>;

const TORRENT_SNAPSHOT_CACHE_SIZE: usize = 4;
const TORRENT_SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(750);
type TorrentOrderCache = StdMutex<HashMap<String, Arc<Vec<usize>>>>;

#[derive(Debug, Clone)]
pub struct TorrentSnapshot {
    pub revision: u64,
    pub entries: Arc<Vec<TorrentEntry>>,
    orders: Arc<TorrentOrderCache>,
    filters: Arc<TorrentFilterIndex>,
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
    by_hash: HashMap<String, usize>,
    by_state: HashMap<String, Arc<Vec<usize>>>,
    by_category: HashMap<String, Arc<Vec<usize>>>,
    by_tag: HashMap<String, Arc<Vec<usize>>>,
    completed: Arc<Vec<usize>>,
}

impl TorrentSnapshot {
    /// Return an immutable index for one qBittorrent sort. The index is
    /// shared by all pages using this snapshot; reverse order is handled by
    /// the caller. The comparator is kept in the handler because qBit state
    /// names are an API concern, not a session concern.
    pub(crate) fn ordered_indices<F>(
        &self,
        requested_sort: Option<&str>,
        compare: F,
    ) -> Arc<Vec<usize>>
    where
        F: Fn(&TorrentEntry, &TorrentEntry) -> Ordering,
    {
        let sort = requested_sort.unwrap_or("name").to_owned();
        if let Some(indices) = self
            .orders
            .lock()
            .expect("qbit torrent snapshot order mutex poisoned")
            .get(&sort)
            .cloned()
        {
            return indices;
        }

        let mut indices = (0..self.entries.len()).collect::<Vec<_>>();
        indices.sort_unstable_by(|left, right| {
            compare(&self.entries[*left], &self.entries[*right]).then_with(|| {
                self.entries[*left]
                    .info_hash
                    .cmp(&self.entries[*right].info_hash)
            })
        });
        let indices = Arc::new(indices);
        self.orders
            .lock()
            .expect("qbit torrent snapshot order mutex poisoned")
            .insert(sort, Arc::clone(&indices));
        indices
    }

    /// Return candidate indexes for exact qBittorrent filters. Substring
    /// filtering is still applied by the handler, but state/category/tag and
    /// completion filters no longer require a full predicate pass.
    pub(crate) fn candidate_indices(
        &self,
        hashes: Option<&HashSet<String>>,
        states: &[&str],
        completed_only: bool,
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
        if let Some(hashes) = hashes {
            let mut indexes = hashes
                .iter()
                .filter_map(|hash| self.filters.by_hash.get(hash).copied())
                .collect::<Vec<_>>();
            indexes.sort_unstable();
            indexes.dedup();
            candidates = Some(match candidates {
                Some(current) => intersect_sorted(&current, &indexes),
                None => indexes,
            });
        }
        if completed_only {
            candidates = Some(match candidates {
                Some(current) => intersect_sorted(&current, &self.filters.completed),
                None => self.filters.completed.as_ref().clone(),
            });
        }
        for indexes in [
            category.and_then(|value| self.filters.by_category.get(value)),
            tag.and_then(|value| self.filters.by_tag.get(value)),
        ]
        .into_iter()
        {
            let Some(indexes) = indexes else {
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

fn build_filter_index(entries: &[TorrentEntry]) -> TorrentFilterIndex {
    let mut by_hash = HashMap::<String, usize>::new();
    let mut by_state = HashMap::<String, Vec<usize>>::new();
    let mut by_category = HashMap::<String, Vec<usize>>::new();
    let mut by_tag = HashMap::<String, Vec<usize>>::new();
    let mut completed = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        by_hash.insert(entry.info_hash.clone(), index);
        by_state
            .entry(entry.state.as_str().to_owned())
            .or_default()
            .push(index);
        if let Some(category) = &entry.category {
            by_category.entry(category.clone()).or_default().push(index);
        }
        let mut seen_tags = HashSet::new();
        for tag in &entry.tags {
            if seen_tags.insert(tag) {
                by_tag.entry(tag.clone()).or_default().push(index);
            }
        }
        if entry.completed_at.is_some() {
            completed.push(index);
        }
    }
    TorrentFilterIndex {
        by_hash,
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
        completed: Arc::new(completed),
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

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<BTreeSet<String>>>,
    pub tracker_projection_cache: Arc<RwLock<HashMap<String, (String, u32)>>>,
    pub preference_overrides: Arc<RwLock<JsonMap>>,
    pub app_cookies: Arc<RwLock<Vec<serde_json::Value>>>,
    pub api_key: Arc<RwLock<Option<String>>>,
    pub global_limits: Arc<RwLock<EngineGlobalLimits>>,
    pub banned_peers: Arc<RwLock<BTreeSet<SocketAddr>>>,
    pub search_plugins: Arc<RwLock<JsonMap>>,
    pub search_jobs: Arc<RwLock<JsonMap>>,
    pub next_search_id: Arc<RwLock<i64>>,
    pub rss_items: Arc<RwLock<JsonMap>>,
    pub rss_rules: Arc<RwLock<JsonMap>>,
    torrent_snapshot_cache: Arc<RwLock<VecDeque<CachedTorrentSnapshot>>>,
    torrent_snapshot_refresh: Arc<Mutex<()>>,
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
            torrent_snapshot_refresh: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_registry(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        AppState {
            registry,
            engine: None,
            api_tokens: Arc::new(Vec::new()),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
            torrent_snapshot_refresh: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self::with_engine_and_tokens(registry, engine, Vec::new())
    }

    pub fn with_engine_and_tokens(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
    ) -> Self {
        AppState {
            registry,
            engine: Some(engine),
            api_tokens: Arc::new(api_tokens),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
            torrent_snapshot_refresh: Arc::new(Mutex::new(())),
        }
    }

    /// Return a bounded, immutable registry snapshot for qBittorrent list
    /// pagination. An explicit revision pins a page sequence; an omitted
    /// revision may reuse a recent snapshot to keep refreshes from cloning
    /// the registry on every request.
    pub(crate) async fn torrent_snapshot(
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

        // Serialize cache refreshes. Without this second check, a burst of
        // list clients would each clone the entire registry at the same time.
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

        let (revision, entries) = {
            let registry = self.registry.read().await;
            (
                registry.revision(),
                Arc::new(registry.iter().cloned().collect::<Vec<_>>()),
            )
        };
        let filters = Arc::new(build_filter_index(&entries));
        let snapshot = TorrentSnapshot {
            revision,
            entries,
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

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
