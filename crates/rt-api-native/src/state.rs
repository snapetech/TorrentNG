use rt_api_model::{
    ApiRuntimeMetrics, ChunkedBitSet, ChunkedVec, IdempotencyStore, TorrentSummary,
};
use std::sync::Mutex as StdMutex;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};

use rt_engine::EngineHandle;
use rt_session::{RegistryChange, SessionRegistry};

pub type JsonMap = BTreeMap<String, serde_json::Value>;

const TORRENT_SNAPSHOT_CACHE_SIZE: usize = 4;
const TORRENT_SNAPSHOT_MAX_AGE: Duration = Duration::from_millis(750);
type TorrentOrderCache = StdMutex<HashMap<&'static str, Arc<Vec<usize>>>>;

#[derive(Debug, Clone)]
pub struct TorrentSnapshot {
    pub revision: u64,
    pub torrents: Arc<ChunkedVec<TorrentSnapshotItem>>,
    orders: Arc<TorrentOrderCache>,
    filters: Arc<TorrentFilterIndex>,
}

#[derive(Debug, Clone)]
pub struct TorrentSnapshotItem {
    pub summary: TorrentSummary,
    pub amount_left: u64,
    name_sort_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TorrentLabelFacet {
    pub name: String,
    pub count: usize,
    pub save_path: Option<String>,
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
    by_media_type: HashMap<String, Arc<ChunkedBitSet>>,
    by_category: HashMap<String, Arc<ChunkedBitSet>>,
    by_tag: HashMap<String, Arc<ChunkedBitSet>>,
    /// Inverted indexes for free-text name/hash filters. Queries still verify
    /// the exact substring in the handler, but a normal search no longer
    /// walks every torrent just to find candidates. The one- and two-byte
    /// indexes matter because clients routinely send short filters.
    by_text_byte: HashMap<u8, Arc<ChunkedBitSet>>,
    by_text_bigram: HashMap<[u8; 2], Arc<ChunkedBitSet>>,
    by_text_trigram: HashMap<[u8; 3], Arc<ChunkedBitSet>>,
}

/// Shared application state threaded through axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
    pub api_tokens: Arc<Vec<String>>,
    pub metrics_include_torrent_ids: bool,
    pub categories: Arc<RwLock<BTreeMap<String, String>>>,
    pub tags: Arc<RwLock<Vec<String>>>,
    pub saved_views: Arc<RwLock<JsonMap>>,
    pub ratio_groups: Arc<RwLock<JsonMap>>,
    pub workflows: Arc<RwLock<JsonMap>>,
    pub workflow_runs: Arc<RwLock<Vec<serde_json::Value>>>,
    pub rss_rules: Arc<RwLock<JsonMap>>,
    pub user_agent: Arc<RwLock<String>>,
    pub(crate) api_metrics: Arc<ApiRuntimeMetrics>,
    pub(crate) idempotency: Arc<IdempotencyStore>,
    /// Serializes read-modify-write operations for the small native control
    /// plane. The database command itself is serialized by the engine actor,
    /// but without this lock two HTTP writers could still lose each other's
    /// JSON map update between the read and the write.
    pub(crate) json_store_write: Arc<Mutex<()>>,
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
            metrics_include_torrent_ids: false,
            categories: Arc::new(RwLock::new(BTreeMap::new())),
            tags: Arc::new(RwLock::new(Vec::new())),
            saved_views: Arc::new(RwLock::new(BTreeMap::new())),
            ratio_groups: Arc::new(RwLock::new(BTreeMap::new())),
            workflows: Arc::new(RwLock::new(BTreeMap::new())),
            workflow_runs: Arc::new(RwLock::new(Vec::new())),
            rss_rules: Arc::new(RwLock::new(BTreeMap::new())),
            user_agent: Arc::new(RwLock::new(rt_engine::peer_id::user_agent())),
            api_metrics,
            idempotency: IdempotencyStore::new(),
            json_store_write: Arc::new(Mutex::new(())),
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

    pub fn with_engine_and_tokens_metrics_config(
        registry: Arc<RwLock<SessionRegistry>>,
        engine: EngineHandle,
        api_tokens: Vec<String>,
        api_metrics: Arc<ApiRuntimeMetrics>,
        include_torrent_ids: bool,
    ) -> Self {
        let mut state = Self::from_parts(registry, Some(engine), api_tokens, api_metrics);
        state.metrics_include_torrent_ids = include_torrent_ids;
        state
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

        let previous = { self.torrent_snapshot_cache.read().await.front().cloned() };
        let (revision, (torrents, filters), incremental) = {
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
                        let (torrents, filters) =
                            apply_snapshot_changes(&registry, &previous, &changes);
                        (Arc::new(torrents), Arc::new(filters))
                    },
                    true,
                )
            } else {
                let session_snapshot = registry.snapshot();
                let torrents = session_snapshot
                    .iter()
                    .map(|entry| TorrentSnapshotItem {
                        summary: torrent_summary(entry),
                        amount_left: entry.amount_left,
                        name_sort_key: entry.name.to_ascii_lowercase(),
                    })
                    .collect::<Vec<_>>();
                let torrents = ChunkedVec::from_vec(torrents);
                let filters = build_filter_index(&torrents);
                (revision, (Arc::new(torrents), Arc::new(filters)), false)
            }
        };
        self.api_metrics.record_snapshot_refresh();
        if incremental {
            self.api_metrics.record_snapshot_incremental_update();
        }
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

fn apply_snapshot_changes(
    registry: &SessionRegistry,
    previous: &TorrentSnapshot,
    changes: &[RegistryChange],
) -> (ChunkedVec<TorrentSnapshotItem>, TorrentFilterIndex) {
    let changed_hashes = changes
        .iter()
        .map(|change| change.info_hash.clone())
        .collect::<HashSet<_>>();
    let mut replacements = Vec::new();
    let mut structural_change = false;
    for info_hash in &changed_hashes {
        match registry.get(info_hash) {
            Some(entry) => {
                let item = TorrentSnapshotItem {
                    summary: torrent_summary(&entry),
                    amount_left: entry.amount_left,
                    name_sort_key: entry.name.to_ascii_lowercase(),
                };
                if let Some(index) = previous.filters.by_hash.get(info_hash).copied() {
                    if let Some(old) = previous.torrents.get(index) {
                        replacements.push((index, old.clone(), item));
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
        // A membership change invalidates positional indexes. Mutating the
        // previous vector in HashSet iteration order can apply a later
        // replacement at a stale index after an earlier removal, producing
        // corrupted rows or an out-of-bounds panic. Rebuild from the
        // registry's deterministic snapshot instead.
        let session_snapshot = registry.snapshot();
        let torrents = session_snapshot
            .iter()
            .map(|entry| TorrentSnapshotItem {
                summary: torrent_summary(entry),
                amount_left: entry.amount_left,
                name_sort_key: entry.name.to_ascii_lowercase(),
            })
            .collect::<Vec<_>>();
        let torrents = ChunkedVec::from_vec(torrents);
        let filters = build_filter_index(&torrents);
        (torrents, filters)
    } else {
        let mut filters = previous.filters.as_ref().clone();
        let mut values = Vec::with_capacity(replacements.len());
        for (index, old, item) in replacements {
            update_filter_index(&mut filters, index, &old, &item, previous.torrents.len());
            values.push((index, item));
        }
        (previous.torrents.replace_many(values), filters)
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
        filter: Option<&str>,
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

        if let Some(category) = category {
            let Some(indexes) = self.filters.by_category.get(category) else {
                return Some(Vec::new());
            };
            candidates = Some(match candidates {
                Some(current) => intersect_sorted(&current, &indexes.indices()),
                None => indexes.indices(),
            });
        }
        if let Some(tag) = tag {
            let Some(indexes) = self.filters.by_tag.get(tag) else {
                return Some(Vec::new());
            };
            candidates = Some(match candidates {
                Some(current) => intersect_sorted(&current, &indexes.indices()),
                None => indexes.indices(),
            });
        }
        if let Some(filter) = filter.map(str::trim).filter(|filter| !filter.is_empty()) {
            let (bytes, bigrams, trigrams) = search_text_ngrams(filter);
            for byte in bytes {
                let Some(indexes) = self.filters.by_text_byte.get(&byte) else {
                    return Some(Vec::new());
                };
                candidates = Some(match candidates {
                    Some(current) => intersect_sorted(&current, &indexes.indices()),
                    None => indexes.indices(),
                });
            }
            for bigram in bigrams {
                let Some(indexes) = self.filters.by_text_bigram.get(&bigram) else {
                    return Some(Vec::new());
                };
                candidates = Some(match candidates {
                    Some(current) => intersect_sorted(&current, &indexes.indices()),
                    None => indexes.indices(),
                });
            }
            for trigram in trigrams {
                let Some(indexes) = self.filters.by_text_trigram.get(&trigram) else {
                    return Some(Vec::new());
                };
                candidates = Some(match candidates {
                    Some(current) => intersect_sorted(&current, &indexes.indices()),
                    None => indexes.indices(),
                });
            }
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
                    .and_then(|index| self.torrents.get(index))
                    .map(|item| item.summary.save_path.clone()),
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

    pub(crate) fn state_counts(&self) -> Vec<(String, usize)> {
        self.filters
            .by_state
            .iter()
            .map(|(name, indexes)| (name.clone(), indexes.count()))
            .collect()
    }

    pub(crate) fn media_type_counts(&self) -> Vec<(String, usize)> {
        self.filters
            .by_media_type
            .iter()
            .map(|(name, indexes)| (name.clone(), indexes.count()))
            .collect()
    }
}

fn build_filter_index(torrents: &ChunkedVec<TorrentSnapshotItem>) -> TorrentFilterIndex {
    let mut by_hash = HashMap::<String, usize>::new();
    let mut by_state = HashMap::<String, Vec<usize>>::new();
    let mut by_media_type = HashMap::<String, Vec<usize>>::new();
    let mut by_category = HashMap::<String, Vec<usize>>::new();
    let mut by_tag = HashMap::<String, Vec<usize>>::new();
    let mut by_text_byte = HashMap::<u8, Vec<usize>>::new();
    let mut by_text_bigram = HashMap::<[u8; 2], Vec<usize>>::new();
    let mut by_text_trigram = HashMap::<[u8; 3], Vec<usize>>::new();
    for (index, item) in torrents.iter().enumerate() {
        by_hash.insert(item.summary.info_hash.clone(), index);
        add_text_ngrams(
            &item.summary.name,
            index,
            &mut by_text_byte,
            &mut by_text_bigram,
            &mut by_text_trigram,
        );
        add_text_ngrams(
            &item.summary.info_hash,
            index,
            &mut by_text_byte,
            &mut by_text_bigram,
            &mut by_text_trigram,
        );
        by_state
            .entry(item.summary.state.clone())
            .or_default()
            .push(index);
        by_media_type
            .entry(infer_media_type(&item.summary.name).to_owned())
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
        by_hash: Arc::new(by_hash),
        by_state: by_state
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_media_type: by_media_type
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_category: by_category
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_tag: by_tag
            .into_iter()
            .map(|(key, indexes)| {
                (
                    key,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_text_byte: by_text_byte
            .into_iter()
            .map(|(byte, indexes)| {
                (
                    byte,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_text_bigram: by_text_bigram
            .into_iter()
            .map(|(bigram, indexes)| {
                (
                    bigram,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
        by_text_trigram: by_text_trigram
            .into_iter()
            .map(|(trigram, indexes)| {
                (
                    trigram,
                    Arc::new(ChunkedBitSet::from_indices(torrents.len(), indexes)),
                )
            })
            .collect(),
    }
}

fn update_filter_index(
    filters: &mut TorrentFilterIndex,
    index: usize,
    old: &TorrentSnapshotItem,
    new: &TorrentSnapshotItem,
    len: usize,
) {
    update_membership(
        &mut filters.by_state,
        Some(old.summary.state.as_str()),
        Some(new.summary.state.as_str()),
        index,
        len,
    );
    update_membership(
        &mut filters.by_media_type,
        Some(infer_media_type(&old.summary.name)),
        Some(infer_media_type(&new.summary.name)),
        index,
        len,
    );
    update_membership(
        &mut filters.by_category,
        old.summary.category.as_deref(),
        new.summary.category.as_deref(),
        index,
        len,
    );

    let old_tags = old
        .summary
        .tags
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let new_tags = new
        .summary
        .tags
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    for tag in old_tags.union(&new_tags) {
        update_membership(
            &mut filters.by_tag,
            old_tags.contains(tag).then_some(*tag),
            new_tags.contains(tag).then_some(*tag),
            index,
            len,
        );
    }

    let (old_bytes, old_bigrams, old_trigrams) = search_text_ngrams(&old.summary.name);
    let (new_bytes, new_bigrams, new_trigrams) = search_text_ngrams(&new.summary.name);
    for byte in old_bytes.union(&new_bytes) {
        update_text_membership(
            &mut filters.by_text_byte,
            old_bytes.contains(byte).then_some(byte),
            new_bytes.contains(byte).then_some(byte),
            index,
            len,
        );
    }
    for bigram in old_bigrams.union(&new_bigrams) {
        update_text_membership(
            &mut filters.by_text_bigram,
            old_bigrams.contains(bigram).then_some(bigram),
            new_bigrams.contains(bigram).then_some(bigram),
            index,
            len,
        );
    }
    for trigram in old_trigrams.union(&new_trigrams) {
        update_text_membership(
            &mut filters.by_text_trigram,
            old_trigrams.contains(trigram).then_some(trigram),
            new_trigrams.contains(trigram).then_some(trigram),
            index,
            len,
        );
    }
}

fn search_text_ngrams(value: &str) -> (HashSet<u8>, HashSet<[u8; 2]>, HashSet<[u8; 3]>) {
    let value = value.to_ascii_lowercase();
    let bytes = value.as_bytes();
    (
        bytes.iter().copied().collect(),
        bytes
            .windows(2)
            .map(|window| [window[0], window[1]])
            .collect(),
        bytes
            .windows(3)
            .map(|window| [window[0], window[1], window[2]])
            .collect(),
    )
}

pub(crate) fn infer_media_type(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if [".mkv", ".mp4", ".avi", ".mov", ".webm"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "video"
    } else if [".flac", ".mp3", ".ogg", ".m4a", ".wav"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "audio"
    } else if [".zip", ".rar", ".7z", ".tar", ".gz"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
    {
        "archive"
    } else {
        "other"
    }
}

fn add_text_ngrams(
    value: &str,
    index: usize,
    bytes: &mut HashMap<u8, Vec<usize>>,
    bigrams: &mut HashMap<[u8; 2], Vec<usize>>,
    trigrams: &mut HashMap<[u8; 3], Vec<usize>>,
) {
    let (value_bytes, value_bigrams, value_trigrams) = search_text_ngrams(value);
    for byte in value_bytes {
        bytes.entry(byte).or_default().push(index);
    }
    for bigram in value_bigrams {
        bigrams.entry(bigram).or_default().push(index);
    }
    for trigram in value_trigrams {
        trigrams.entry(trigram).or_default().push(index);
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

fn update_text_membership<K>(
    index: &mut HashMap<K, Arc<ChunkedBitSet>>,
    old: Option<&K>,
    new: Option<&K>,
    position: usize,
    len: usize,
) where
    K: Clone + Eq + std::hash::Hash,
{
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
            .entry(new.clone())
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
            let left = self.torrents.get(*left).expect("snapshot index is valid");
            let right = self.torrents.get(*right).expect("snapshot index is valid");
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
        total_length: native_i64(entry.total_length),
        downloaded: native_i64(entry.stats.downloaded),
        uploaded: native_i64(entry.stats.uploaded),
        ratio: entry.stats.ratio(),
        save_path: entry.save_path.clone(),
        category: entry.category.clone(),
        tags: entry.tags.clone(),
        added_at: native_i64(entry.added_at),
        completed_at: entry.completed_at.map(native_i64),
        num_peers: 0,
        num_seeds: 0,
        tracker_message: entry.tracker_message.clone(),
    }
}

/// API models use signed counters for compatibility with the existing wire
/// contract. Never let a persisted or engine-owned u64 wrap into a negative
/// value when projecting it to that contract.
pub(crate) fn native_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn native_usize_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_session::TorrentEntry;

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
        let previous_items = registry
            .iter()
            .map(|entry| TorrentSnapshotItem {
                summary: torrent_summary(&entry),
                amount_left: entry.amount_left,
                name_sort_key: entry.name.to_ascii_lowercase(),
            })
            .collect::<Vec<_>>();
        let previous_items = ChunkedVec::from_vec(previous_items.clone());
        let previous = TorrentSnapshot {
            revision: registry.revision(),
            torrents: Arc::new(previous_items.clone()),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(build_filter_index(&previous_items)),
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
        assert_eq!(refreshed.0.get(0).unwrap().summary.info_hash, first_hash);
        assert_eq!(refreshed.0.get(0).unwrap().summary.name, "after");
        let refreshed_snapshot = TorrentSnapshot {
            revision: registry.revision(),
            torrents: Arc::new(refreshed.0),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(refreshed.1),
        };
        assert_eq!(
            refreshed_snapshot.candidate_indices(&[], None, None, Some("after")),
            Some(vec![0])
        );
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
        let session_snapshot = registry.snapshot();
        let previous_items = session_snapshot
            .iter()
            .map(|entry| TorrentSnapshotItem {
                summary: torrent_summary(entry),
                amount_left: entry.amount_left,
                name_sort_key: entry.name.to_ascii_lowercase(),
            })
            .collect::<Vec<_>>();
        let previous_items = ChunkedVec::from_vec(previous_items);
        let previous = TorrentSnapshot {
            revision: registry.revision(),
            torrents: Arc::new(previous_items.clone()),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(build_filter_index(&previous_items)),
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
        let (torrents, _) = apply_snapshot_changes(&registry, &previous, &changes);
        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents.get(0).unwrap().summary.info_hash, first_hash);
        assert_eq!(torrents.get(0).unwrap().summary.name, "after");
    }

    #[test]
    fn signed_summary_projection_saturates_unsigned_counters() {
        assert_eq!(native_i64(i64::MAX as u64), i64::MAX);
        assert_eq!(native_i64(i64::MAX as u64 + 1), i64::MAX);
        assert_eq!(native_usize_i64(usize::MAX), i64::MAX);
    }

    #[test]
    fn text_filter_index_handles_short_filters_and_case_folding() {
        let first_hash = "a".repeat(40);
        let second_hash = "b".repeat(40);
        let mut registry = SessionRegistry::new();
        registry
            .add(TorrentEntry::new(
                first_hash,
                "Alpha Release".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();
        registry
            .add(TorrentEntry::new(
                second_hash,
                "Zulu Release".to_owned(),
                "/data".to_owned(),
            ))
            .unwrap();

        let items = registry
            .iter()
            .map(|entry| TorrentSnapshotItem {
                summary: torrent_summary(&entry),
                amount_left: entry.amount_left,
                name_sort_key: entry.name.to_ascii_lowercase(),
            })
            .collect::<Vec<_>>();
        let items = ChunkedVec::from_vec(items);
        let snapshot = TorrentSnapshot {
            revision: registry.revision(),
            torrents: Arc::new(items.clone()),
            orders: Arc::new(StdMutex::new(HashMap::new())),
            filters: Arc::new(build_filter_index(&items)),
        };
        let alpha_index = snapshot
            .torrents
            .iter()
            .position(|item| item.summary.name == "Alpha Release")
            .unwrap();
        let zulu_index = snapshot
            .torrents
            .iter()
            .position(|item| item.summary.name == "Zulu Release")
            .unwrap();

        assert_eq!(
            snapshot.candidate_indices(&[], None, None, Some("P")),
            Some(vec![alpha_index])
        );
        assert_eq!(
            snapshot.candidate_indices(&[], None, None, Some("ph")),
            Some(vec![alpha_index])
        );
        let release_candidates = snapshot
            .candidate_indices(&[], None, None, Some("release"))
            .unwrap();
        assert_eq!(
            release_candidates,
            vec![alpha_index.min(zulu_index), alpha_index.max(zulu_index)]
        );
        assert_eq!(
            snapshot.candidate_indices(&[], None, None, Some("qq")),
            Some(Vec::new())
        );
        assert_eq!(
            snapshot.candidate_indices(&[], None, None, Some("   ")),
            None
        );
    }

    #[test]
    fn media_type_facets_are_indexed_and_updated_incrementally() {
        let summary = |name: &str| TorrentSummary {
            info_hash: name.to_owned(),
            name: name.to_owned(),
            state: "stopped".to_owned(),
            total_length: 0,
            downloaded: 0,
            uploaded: 0,
            ratio: 0.0,
            save_path: "/data".to_owned(),
            category: None,
            tags: Vec::new(),
            added_at: 0,
            completed_at: None,
            num_peers: 0,
            num_seeds: 0,
            tracker_message: None,
        };
        let items = ChunkedVec::from_vec(vec![
            TorrentSnapshotItem {
                summary: summary("movie.mkv"),
                amount_left: 0,
                name_sort_key: "movie.mkv".to_owned(),
            },
            TorrentSnapshotItem {
                summary: summary("song.mp3"),
                amount_left: 0,
                name_sort_key: "song.mp3".to_owned(),
            },
        ]);
        let mut filters = build_filter_index(&items);
        let counts = |filters: &TorrentFilterIndex| {
            filters
                .by_media_type
                .iter()
                .map(|(name, indexes)| (name.clone(), indexes.count()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(counts(&filters).get("video"), Some(&1));
        assert_eq!(counts(&filters).get("audio"), Some(&1));

        let old = items.get(1).unwrap().clone();
        let new = TorrentSnapshotItem {
            summary: TorrentSummary {
                name: "archive.zip".to_owned(),
                ..old.summary.clone()
            },
            amount_left: old.amount_left,
            name_sort_key: "archive.zip".to_owned(),
        };
        update_filter_index(&mut filters, 1, &old, &new, items.len());
        assert_eq!(counts(&filters).get("audio"), Some(&0));
        assert_eq!(counts(&filters).get("archive"), Some(&1));
    }
}
