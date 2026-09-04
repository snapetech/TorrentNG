use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify, RwLock};

use rt_api_model::{ApiRuntimeMetrics, ChunkedBitSet, ChunkedVec, IdempotencyStore};
use rt_engine::{EngineGlobalLimits, EngineHandle, OutboundEgressPolicy};
use rt_session::{RegistryChange, SessionRegistry, TorrentEntry};

pub type JsonMap = serde_json::Map<String, serde_json::Value>;

const TORRENT_SNAPSHOT_CACHE_SIZE: usize = 4;
const TORRENT_SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(750);
type TorrentOrderCache = StdMutex<HashMap<String, Arc<Vec<usize>>>>;

#[derive(Debug, Clone)]
pub struct TorrentSnapshot {
    pub revision: u64,
    pub entries: Arc<ChunkedVec<TorrentEntry>>,
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

#[derive(Debug, Clone)]
struct TorrentFilterIndex {
    by_hash: Arc<HashMap<String, usize>>,
    by_state: HashMap<String, Arc<ChunkedBitSet>>,
    by_category: HashMap<String, Arc<ChunkedBitSet>>,
    by_tag: HashMap<String, Arc<ChunkedBitSet>>,
    completed: Arc<ChunkedBitSet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TorrentLabelFacet {
    pub name: String,
    pub count: usize,
    pub save_path: Option<String>,
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
        let sort = canonical_sort_key(requested_sort).to_owned();
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
            let left = self.entries.get(*left).expect("snapshot index is valid");
            let right = self.entries.get(*right).expect("snapshot index is valid");
            compare(left, right).then_with(|| left.info_hash.cmp(&right.info_hash))
        });
        let indices = Arc::new(indices);
        self.orders
            .lock()
            .expect("qbit torrent snapshot order mutex poisoned")
            .insert(sort, Arc::clone(&indices));
        indices
    }

    /// Return candidate indexes for exact qBittorrent filters. State,
    /// category, tag, and completion filters no longer require a full
    /// predicate pass.
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
                    union.extend(indexes.indices());
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
                Some(current) => intersect_sorted(&current, &self.filters.completed.indices()),
                None => self.filters.completed.indices(),
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
                Some(current) => intersect_sorted(&current, &indexes.indices()),
                None => indexes.indices(),
            });
        }
        if category.is_some_and(|value| !self.filters.by_category.contains_key(value))
            || tag.is_some_and(|value| !self.filters.by_tag.contains_key(value))
        {
            return Some(Vec::new());
        }
        candidates
    }

    pub(crate) fn category_facets(&self) -> Vec<TorrentLabelFacet> {
        self.filters
            .by_category
            .iter()
            .map(|(name, indexes)| TorrentLabelFacet {
                name: name.clone(),
                count: indexes.count(),
                save_path: indexes
                    .first_index()
                    .and_then(|index| self.entries.get(index))
                    .map(|entry| entry.save_path.clone()),
            })
            .collect()
    }

    pub(crate) fn tag_facets(&self) -> Vec<(String, usize)> {
        self.filters
            .by_tag
            .iter()
            .map(|(name, indexes)| (name.clone(), indexes.count()))
            .collect()
    }
}

/// qBittorrent accepts a free-form `sort` query parameter, but TorrentNG can
/// only provide stable ordering for the fields represented by `TorrentEntry`.
/// Normalize unknown values to the documented name order before using the
/// value as a snapshot cache key; otherwise one request per arbitrary string
/// retains another full index for the lifetime of the snapshot.
pub(crate) fn canonical_sort_key(requested_sort: Option<&str>) -> &'static str {
    match requested_sort.map(str::trim) {
        Some("name") => "name",
        Some("hash") => "hash",
        Some("size") => "size",
        Some("progress") => "progress",
        Some("ratio") => "ratio",
        Some("added_on") => "added_on",
        Some("completion_on") => "completion_on",
        Some("category") => "category",
        Some("state") => "state",
        _ => "name",
    }
}

fn build_filter_index(entries: &ChunkedVec<TorrentEntry>) -> TorrentFilterIndex {
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
        by_hash: Arc::new(by_hash),
        by_state: by_state
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(entries.len(), indexes)),
                )
            })
            .collect(),
        by_category: by_category
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(entries.len(), indexes)),
                )
            })
            .collect(),
        by_tag: by_tag
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(entries.len(), indexes)),
                )
            })
            .collect(),
        completed: Arc::new(ChunkedBitSet::from_indices(entries.len(), completed)),
    }
}

fn update_filter_index(
    filters: &mut TorrentFilterIndex,
    index: usize,
    old: &TorrentEntry,
    new: &TorrentEntry,
    len: usize,
) {
    update_membership(
        &mut filters.by_state,
        Some(old.state.as_str()),
        Some(new.state.as_str()),
        index,
        len,
    );
    update_membership(
        &mut filters.by_category,
        old.category.as_deref(),
        new.category.as_deref(),
        index,
        len,
    );

    let old_tags = old.tags.iter().map(String::as_str).collect::<HashSet<_>>();
    let new_tags = new.tags.iter().map(String::as_str).collect::<HashSet<_>>();
    for tag in old_tags.union(&new_tags) {
        update_membership(
            &mut filters.by_tag,
            old_tags.contains(tag).then_some(*tag),
            new_tags.contains(tag).then_some(*tag),
            index,
            len,
        );
    }
    if old.completed_at != new.completed_at {
        filters.completed = Arc::new(filters.completed.set(index, new.completed_at.is_some()));
    }
}

fn update_membership(
    index: &mut HashMap<String, Arc<ChunkedBitSet>>,
    old: Option<&str>,
    new: Option<&str>,
    position: usize,
    len: usize,
) {
    if old == new {
        return;
    }
    if let Some(old) = old {
        if let Some(bits) = index.get_mut(old) {
            *bits = Arc::new(bits.set(position, false));
        }
    }
    if let Some(new) = new {
        let bits = index
            .entry(new.to_owned())
            .or_insert_with(|| Arc::new(ChunkedBitSet::empty(len)));
        *bits = Arc::new(bits.set(position, true));
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
    /// Daemon-owned shutdown signal. The qBittorrent endpoint only requests
    /// shutdown; the daemon remains responsible for running the engine's
    /// graceful shutdown sequence.
    pub shutdown: Option<Arc<Notify>>,
    pub api_tokens: Arc<Vec<String>>,
    pub egress_policy: OutboundEgressPolicy,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<BTreeSet<String>>>,
    pub tracker_projection_cache: Arc<RwLock<HashMap<String, (String, u32)>>>,
    pub preference_overrides: Arc<RwLock<JsonMap>>,
    pub app_cookies: Arc<RwLock<Vec<serde_json::Value>>>,
    pub api_key: Arc<RwLock<Option<String>>>,
    /// Serializes qBittorrent compatibility read-modify-write operations.
    /// The engine actor serializes individual setting writes, but it cannot
    /// prevent two HTTP requests from reading the same JSON blob and losing
    /// one another's updates before either write reaches the actor.
    pub(crate) preference_write: Arc<Mutex<()>>,
    pub global_limits: Arc<RwLock<EngineGlobalLimits>>,
    /// In-memory-only limit projection used by facade tests and embedders that
    /// intentionally do not attach the native engine. The daemon always uses
    /// the durable engine path instead.
    pub torrent_limits: Arc<RwLock<HashMap<String, rt_engine::EngineTorrentLimits>>>,
    pub banned_peers: Arc<RwLock<BTreeSet<SocketAddr>>>,
    pub(crate) api_metrics: Arc<ApiRuntimeMetrics>,
    pub(crate) idempotency: Arc<IdempotencyStore>,
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
            shutdown: None,
            api_tokens: Arc::new(Vec::new()),
            egress_policy: OutboundEgressPolicy::default(),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            preference_write: Arc::new(Mutex::new(())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            torrent_limits: Arc::new(RwLock::new(HashMap::new())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            api_metrics: ApiRuntimeMetrics::new(),
            idempotency: IdempotencyStore::new(),
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
            shutdown: None,
            api_tokens: Arc::new(Vec::new()),
            egress_policy: OutboundEgressPolicy::default(),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            preference_write: Arc::new(Mutex::new(())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            torrent_limits: Arc::new(RwLock::new(HashMap::new())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            api_metrics: ApiRuntimeMetrics::new(),
            idempotency: IdempotencyStore::new(),
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
            shutdown: None,
            api_tokens: Arc::new(api_tokens),
            egress_policy: OutboundEgressPolicy::default(),
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(BTreeSet::new())),
            tracker_projection_cache: Arc::new(RwLock::new(HashMap::new())),
            preference_overrides: Arc::new(RwLock::new(serde_json::Map::new())),
            app_cookies: Arc::new(RwLock::new(Vec::new())),
            api_key: Arc::new(RwLock::new(None)),
            preference_write: Arc::new(Mutex::new(())),
            global_limits: Arc::new(RwLock::new(EngineGlobalLimits::default())),
            torrent_limits: Arc::new(RwLock::new(HashMap::new())),
            banned_peers: Arc::new(RwLock::new(BTreeSet::new())),
            api_metrics: ApiRuntimeMetrics::new(),
            idempotency: IdempotencyStore::new(),
            search_plugins: Arc::new(RwLock::new(serde_json::Map::new())),
            search_jobs: Arc::new(RwLock::new(serde_json::Map::new())),
            next_search_id: Arc::new(RwLock::new(1)),
            rss_items: Arc::new(RwLock::new(serde_json::Map::new())),
            rss_rules: Arc::new(RwLock::new(serde_json::Map::new())),
            torrent_snapshot_cache: Arc::new(RwLock::new(VecDeque::new())),
            torrent_snapshot_refresh: Arc::new(Mutex::new(())),
        }
    }

    pub fn with_engine_and_tokens_and_metrics(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
        api_metrics: Arc<ApiRuntimeMetrics>,
    ) -> Self {
        let mut state = Self::with_engine_and_tokens(registry, engine, api_tokens);
        state.api_metrics = api_metrics;
        state
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

        let previous = { self.torrent_snapshot_cache.read().await.front().cloned() };
        let (revision, (entries, filters), incremental) = {
            let registry = self.registry.read().await;
            let revision = registry.revision();
            let incremental = previous
                .filter(|cached| cached.snapshot.revision < revision)
                .and_then(|cached| {
                    registry
                        .changes_since(cached.snapshot.revision)
                        .map(|changes| (cached.snapshot, changes))
                });
            if let Some((previous, changes)) = incremental {
                (
                    revision,
                    {
                        let (entries, filters) =
                            apply_snapshot_changes(&registry, &previous, &changes);
                        (Arc::new(entries), Arc::new(filters))
                    },
                    true,
                )
            } else {
                let entries = ChunkedVec::from_vec(registry.snapshot().iter().cloned().collect());
                (
                    revision,
                    (
                        Arc::new(entries.clone()),
                        Arc::new(build_filter_index(&entries)),
                    ),
                    false,
                )
            }
        };
        self.api_metrics.record_snapshot_refresh();
        if incremental {
            self.api_metrics.record_snapshot_incremental_update();
        }
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

fn apply_snapshot_changes(
    registry: &SessionRegistry,
    previous: &TorrentSnapshot,
    changes: &[RegistryChange],
) -> (ChunkedVec<TorrentEntry>, TorrentFilterIndex) {
    let changed_hashes = changes
        .iter()
        .map(|change| change.info_hash.clone())
        .collect::<HashSet<_>>();
    let mut replacements = Vec::new();
    let mut structural_change = false;
    for info_hash in &changed_hashes {
        match registry.get(info_hash) {
            Some(entry) => {
                if let Some(index) = previous.filters.by_hash.get(info_hash).copied() {
                    if let Some(old) = previous.entries.get(index) {
                        replacements.push((index, old.clone(), entry));
                    }
                } else {
                    structural_change = true;
                }
            }
            None => {
                if previous.filters.by_hash.contains_key(info_hash) {
                    structural_change = true;
                }
            }
        }
    }

    if structural_change {
        // Membership changes invalidate every positional index. Applying
        // removals and replacements to the previous vector in HashSet order
        // can address a stale position after a removal, corrupting the page
        // or panicking. Rebuild from the deterministic registry projection.
        let entries = registry.snapshot().iter().cloned().collect::<Vec<_>>();
        let entries = ChunkedVec::from_vec(entries);
        let filters = build_filter_index(&entries);
        (entries, filters)
    } else {
        let mut filters = previous.filters.as_ref().clone();
        let mut values = Vec::with_capacity(replacements.len());
        for (index, old, entry) in replacements {
            update_filter_index(&mut filters, index, &old, &entry, previous.entries.len());
            values.push((index, entry));
        }
        (previous.entries.replace_many(values), filters)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_refresh_applies_final_entry_state_without_registry_scan() {
        let first_hash = "a".repeat(40);
        let second_hash = "b".repeat(40);
        let mut registry = SessionRegistry::new();
        registry
            .add(TorrentEntry::new(
                first_hash.clone(),
                "before".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();
        let previous_entries = registry.iter().collect::<Vec<_>>();
        let previous_entries = ChunkedVec::from_vec(previous_entries.clone());
        let previous = TorrentSnapshot {
            revision: registry.revision(),
            entries: Arc::new(previous_entries.clone()),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(build_filter_index(&previous_entries)),
        };

        registry
            .add(TorrentEntry::new(
                second_hash.clone(),
                "removed".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();
        {
            let mut entry = registry.get_mut(&first_hash).unwrap();
            entry.name = "after".to_owned();
        }
        registry.remove(&second_hash).unwrap();

        let changes = registry.changes_since(previous.revision).unwrap();
        let refreshed = apply_snapshot_changes(&registry, &previous, &changes);
        assert_eq!(refreshed.0.len(), 1);
        assert_eq!(refreshed.0.get(0).unwrap().info_hash, first_hash);
        assert_eq!(refreshed.0.get(0).unwrap().name, "after");
    }

    #[test]
    fn structural_snapshot_refresh_does_not_use_stale_positions() {
        let first_hash = "a".repeat(40);
        let second_hash = "b".repeat(40);
        let mut registry = SessionRegistry::new();
        registry
            .add(TorrentEntry::new(
                first_hash.clone(),
                "before".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();
        registry
            .add(TorrentEntry::new(
                second_hash.clone(),
                "removed".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();
        let previous_entries = registry.snapshot().iter().cloned().collect::<Vec<_>>();
        let previous_entries = ChunkedVec::from_vec(previous_entries.clone());
        let previous = TorrentSnapshot {
            revision: registry.revision(),
            entries: Arc::new(previous_entries.clone()),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(build_filter_index(&previous_entries)),
        };

        registry.get_mut(&first_hash).unwrap().name = "after".to_owned();
        registry.remove(&second_hash).unwrap();

        // Deliberately place the removal before the replacement. The old
        // positional implementation could index past the shortened vector.
        let changes = vec![
            RegistryChange {
                revision: previous.revision + 1,
                info_hash: second_hash,
                removed: true,
            },
            RegistryChange {
                revision: previous.revision + 2,
                info_hash: first_hash.clone(),
                removed: false,
            },
        ];
        let (entries, _) = apply_snapshot_changes(&registry, &previous, &changes);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.get(0).unwrap().info_hash, first_hash);
        assert_eq!(entries.get(0).unwrap().name, "after");
    }

    #[test]
    fn arbitrary_sort_values_do_not_grow_snapshot_order_cache() {
        let entries = ChunkedVec::from_vec(vec![TorrentEntry::new(
            "a".repeat(40),
            "alpha".to_owned(),
            "/data".to_owned(),
        )]);
        let snapshot = TorrentSnapshot {
            revision: 1,
            entries: Arc::new(entries),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(TorrentFilterIndex {
                by_hash: Arc::new(HashMap::new()),
                by_state: HashMap::new(),
                by_category: HashMap::new(),
                by_tag: HashMap::new(),
                completed: Arc::new(ChunkedBitSet::empty(1)),
            }),
        };

        for index in 0..1_000 {
            let requested = format!("attacker-sort-{index}");
            snapshot.ordered_indices(Some(&requested), |left, right| left.name.cmp(&right.name));
        }

        assert_eq!(
            snapshot
                .orders
                .lock()
                .expect("order cache mutex poisoned")
                .len(),
            1
        );
    }
}
