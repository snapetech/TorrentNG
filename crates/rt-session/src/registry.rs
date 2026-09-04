/// In-memory registry of all active torrent sessions.
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::{
    error::SessionError,
    state::TorrentState,
    torrent::{DormantTorrent, TorrentEntry, TorrentHandle},
};

/// A registry record is either a fully materialized runtime entry or the
/// compact durable projection used by taskless/dormant torrents.
#[derive(Debug, Clone)]
enum RegistryRecord {
    Active(TorrentEntry),
    Dormant(DormantTorrent),
}

impl RegistryRecord {
    fn info_hash(&self) -> &str {
        match self {
            Self::Active(entry) => &entry.info_hash,
            Self::Dormant(entry) => &entry.info_hash,
        }
    }

    fn contribution(&self) -> EntryContribution {
        match self {
            Self::Active(entry) => EntryContribution::from_entry(entry),
            Self::Dormant(entry) => {
                let (state, uploaded, downloaded, amount_left) = entry.contribution();
                EntryContribution {
                    state,
                    uploaded,
                    downloaded,
                    amount_left,
                    total_length: entry.total_length,
                }
            }
        }
    }

    fn to_entry(&self) -> TorrentEntry {
        match self {
            Self::Active(entry) => entry.clone(),
            Self::Dormant(entry) => entry.to_entry(),
        }
    }

    fn state(&self) -> TorrentState {
        match self {
            Self::Active(entry) => entry.state,
            Self::Dormant(entry) => entry.state,
        }
    }
}

pub struct SessionRegistry {
    /// Keyed by hex infohash for O(1) lookup by hash.
    by_hash: HashMap<String, TorrentHandle>,
    entries: HashMap<TorrentHandle, RegistryRecord>,
    /// Monotonic generation for snapshot consumers. Mutable access advances
    /// this when the guard is actually dereferenced mutably; callers therefore
    /// cannot publish a snapshot that predates a completed update, while
    /// read-only borrows do not churn the journal.
    revision: u64,
    changes: VecDeque<RegistryChange>,
    stats: SessionRegistryStats,
    active_count: usize,
    dormant_count: usize,
    /// Process-wide peer bans shared by every torrent task. Keeping this
    /// policy beside the registry means active tasks and taskless promotion
    /// paths consult the same authoritative set without another engine-wide
    /// mutable singleton.
    banned_peers: HashSet<SocketAddr>,
    change_notify: Arc<Notify>,
    snapshot_cache: Mutex<Option<(u64, Arc<Vec<TorrentEntry>>)>>,
}

const CHANGE_LOG_CAPACITY: usize = 16_384;
pub const MAX_BANNED_PEERS: usize = 65_536;

/// A compact mutation record for incremental API/SSE consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryChange {
    pub revision: u64,
    pub info_hash: String,
    pub removed: bool,
}

/// O(1) aggregate counters for hot API and engine-stat paths. The counters
/// are maintained when entries enter, leave, or finish a mutable borrow, so
/// stats collection does not need to walk every torrent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionRegistryStats {
    pub torrents_total: u64,
    pub torrents_stopped: u64,
    pub torrents_metadata_pending: u64,
    pub torrents_checking: u64,
    pub torrents_seeding: u64,
    pub torrents_downloading: u64,
    pub torrents_paused: u64,
    pub torrents_queued: u64,
    pub torrents_error: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub bytes_left: u64,
}

/// A deterministic immutable projection for compatibility read paths.
/// Building this view is proportional to the registry size, but the result
/// is shared until the registry revision changes instead of being rebuilt by
/// every Deluge/Transmission request.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    revision: u64,
    entries: Arc<Vec<TorrentEntry>>,
}

impl SessionSnapshot {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&TorrentEntry> {
        self.entries.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TorrentEntry> {
        self.entries.iter()
    }

    pub fn find(&self, info_hash: &str) -> Option<&TorrentEntry> {
        let info_hash = canonical_info_hash(info_hash);
        self.entries
            .binary_search_by(|entry| entry.info_hash.as_str().cmp(&info_hash))
            .ok()
            .and_then(|index| self.entries.get(index))
    }
}

/// Infohashes are hexadecimal identifiers and are case-insensitive on the
/// wire. Keep the internal registry key canonical while preserving the
/// existing treatment of symbolic test/job identifiers.
fn canonical_info_hash(info_hash: &str) -> String {
    if matches!(info_hash.len(), 40 | 64) && info_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        info_hash.to_ascii_lowercase()
    } else {
        info_hash.to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryContribution {
    state: TorrentState,
    uploaded: u64,
    downloaded: u64,
    total_length: u64,
    amount_left: u64,
}

impl EntryContribution {
    fn from_entry(entry: &TorrentEntry) -> Self {
        Self {
            state: entry.state,
            uploaded: entry.stats.uploaded,
            downloaded: entry.stats.downloaded,
            total_length: entry.total_length,
            amount_left: entry.amount_left,
        }
    }
}

impl SessionRegistryStats {
    fn add_contribution(&mut self, contribution: EntryContribution) {
        self.torrents_total = self.torrents_total.saturating_add(1);
        self.add_state(contribution.state, 1);
        self.bytes_uploaded = self.bytes_uploaded.saturating_add(contribution.uploaded);
        self.bytes_downloaded = self
            .bytes_downloaded
            .saturating_add(contribution.downloaded);
        self.bytes_total = self.bytes_total.saturating_add(contribution.total_length);
        self.bytes_left = self.bytes_left.saturating_add(contribution.amount_left);
    }

    fn remove_contribution(&mut self, contribution: EntryContribution) {
        self.torrents_total = self.torrents_total.saturating_sub(1);
        self.add_state(contribution.state, -1);
        self.bytes_uploaded = self.bytes_uploaded.saturating_sub(contribution.uploaded);
        self.bytes_downloaded = self
            .bytes_downloaded
            .saturating_sub(contribution.downloaded);
        self.bytes_total = self.bytes_total.saturating_sub(contribution.total_length);
        self.bytes_left = self.bytes_left.saturating_sub(contribution.amount_left);
    }

    fn apply_delta(&mut self, before: EntryContribution, after: EntryContribution) {
        if before.state != after.state {
            self.add_state(before.state, -1);
            self.add_state(after.state, 1);
        }
        self.bytes_uploaded = adjust_counter(self.bytes_uploaded, before.uploaded, after.uploaded);
        self.bytes_downloaded =
            adjust_counter(self.bytes_downloaded, before.downloaded, after.downloaded);
        self.bytes_total =
            adjust_counter(self.bytes_total, before.total_length, after.total_length);
        self.bytes_left = adjust_counter(self.bytes_left, before.amount_left, after.amount_left);
    }

    fn add_state(&mut self, state: TorrentState, delta: i8) {
        let counter = match state {
            TorrentState::Stopped => &mut self.torrents_stopped,
            TorrentState::MetadataPending => &mut self.torrents_metadata_pending,
            TorrentState::Checking => &mut self.torrents_checking,
            TorrentState::Seeding => &mut self.torrents_seeding,
            TorrentState::Downloading => &mut self.torrents_downloading,
            TorrentState::Paused => &mut self.torrents_paused,
            TorrentState::Queued => &mut self.torrents_queued,
            TorrentState::Error => &mut self.torrents_error,
        };
        if delta.is_positive() {
            *counter = counter.saturating_add(delta as u64);
        } else {
            *counter = counter.saturating_sub(delta.unsigned_abs() as u64);
        }
    }
}

fn adjust_counter(current: u64, before: u64, after: u64) -> u64 {
    if after >= before {
        current.saturating_add(after - before)
    } else {
        current.saturating_sub(before - after)
    }
}

/// Mutable registry access that reconciles aggregate counters when the
/// borrow ends. Keeping the reconciliation at this boundary prevents a
/// caller from silently invalidating O(1) stats by changing an entry in place.
pub struct SessionRegistryEntryMut<'a> {
    entry: &'a mut TorrentEntry,
    aggregate: &'a mut SessionRegistryStats,
    revision: &'a mut u64,
    changes: &'a mut VecDeque<RegistryChange>,
    change_notify: &'a Notify,
    info_hash: String,
    before: EntryContribution,
    touched: bool,
}

impl Deref for SessionRegistryEntryMut<'_> {
    type Target = TorrentEntry;

    fn deref(&self) -> &Self::Target {
        self.entry
    }
}

impl DerefMut for SessionRegistryEntryMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // DerefMut is invoked for assignments and `&mut self` method calls,
        // including mutations to fields that are not part of the aggregate
        // counters (name, labels, save path, and messages). Tracking this at
        // the access boundary avoids cloning the whole TorrentEntry merely
        // to detect whether a mutable borrow changed anything.
        self.touched = true;
        self.entry
    }
}

impl Drop for SessionRegistryEntryMut<'_> {
    fn drop(&mut self) {
        if !self.touched {
            return;
        }
        let after = EntryContribution::from_entry(self.entry);
        self.aggregate.apply_delta(self.before, after);
        *self.revision = self.revision.wrapping_add(1);
        self.changes.push_back(RegistryChange {
            revision: *self.revision,
            info_hash: self.info_hash.clone(),
            removed: false,
        });
        while self.changes.len() > CHANGE_LOG_CAPACITY {
            self.changes.pop_front();
        }
        self.change_notify.notify_waiters();
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        SessionRegistry {
            by_hash: HashMap::new(),
            entries: HashMap::new(),
            revision: 0,
            changes: VecDeque::new(),
            stats: SessionRegistryStats::default(),
            active_count: 0,
            dormant_count: 0,
            banned_peers: HashSet::new(),
            change_notify: Arc::new(Notify::new()),
            snapshot_cache: Mutex::new(None),
        }
    }

    pub fn add(&mut self, entry: TorrentEntry) -> Result<TorrentHandle, SessionError> {
        self.add_record(RegistryRecord::Active(entry))
    }

    /// Add a taskless torrent without materializing the full runtime entry.
    /// The durable projection remains available through `get`/`iter`, while
    /// `get_mut` promotes it only when a mutation actually needs a mutable
    /// `TorrentEntry`.
    pub fn add_dormant(&mut self, entry: DormantTorrent) -> Result<TorrentHandle, SessionError> {
        self.add_record(RegistryRecord::Dormant(entry))
    }

    fn add_record(&mut self, record: RegistryRecord) -> Result<TorrentHandle, SessionError> {
        let mut record = record;
        let canonical = canonical_info_hash(record.info_hash());
        if canonical != record.info_hash() {
            match &mut record {
                RegistryRecord::Active(entry) => entry.info_hash = canonical,
                RegistryRecord::Dormant(entry) => {
                    entry.info_hash = Arc::<str>::from(canonical);
                }
            }
        }
        let info_hash = record.info_hash().to_owned();
        if self.by_hash.contains_key(&info_hash) {
            return Err(SessionError::AlreadyExists(info_hash));
        }
        let handle = match &record {
            RegistryRecord::Active(entry) => entry.handle,
            RegistryRecord::Dormant(entry) => entry.handle,
        };
        self.by_hash.insert(info_hash.clone(), handle);
        self.stats.add_contribution(record.contribution());
        match &record {
            RegistryRecord::Active(_) => self.active_count += 1,
            RegistryRecord::Dormant(_) => self.dormant_count += 1,
        }
        self.entries.insert(handle, record);
        self.bump_revision(info_hash, false);
        Ok(handle)
    }

    pub fn remove(&mut self, info_hash: &str) -> Result<TorrentEntry, SessionError> {
        let info_hash = canonical_info_hash(info_hash);
        let handle = self
            .by_hash
            .remove(&info_hash)
            .ok_or_else(|| SessionError::NotFound(info_hash.clone()))?;
        let record = self.entries.remove(&handle).unwrap();
        let info_hash = record.info_hash().to_owned();
        let removed_entry = record.to_entry();
        self.stats.remove_contribution(record.contribution());
        match &record {
            RegistryRecord::Active(_) => self.active_count -= 1,
            RegistryRecord::Dormant(_) => self.dormant_count -= 1,
        }
        self.bump_revision(info_hash, true);
        Ok(removed_entry)
    }

    /// Replace an active record with its compact dormant form without
    /// changing the externally visible projection or revision.  The engine
    /// calls this only after the runtime task has stopped and has already
    /// persisted the current state.
    pub fn demote(&mut self, info_hash: &str) -> Result<bool, SessionError> {
        let info_hash = canonical_info_hash(info_hash);
        let handle = *self
            .by_hash
            .get(&info_hash)
            .ok_or_else(|| SessionError::NotFound(info_hash.clone()))?;
        let record = self.entries.remove(&handle).unwrap();
        match record {
            RegistryRecord::Active(entry) => {
                self.entries
                    .insert(handle, RegistryRecord::Dormant(entry.into_dormant()));
                self.active_count -= 1;
                self.dormant_count += 1;
                Ok(true)
            }
            dormant @ RegistryRecord::Dormant(_) => {
                self.entries.insert(handle, dormant);
                Ok(false)
            }
        }
    }

    /// Return an owned projection of a torrent.  Dormant records are
    /// materialized lazily and do not become active merely because they were
    /// read.  Callers that need mutation should use `get_mut`, which promotes
    /// the record explicitly.
    pub fn get(&self, info_hash: &str) -> Option<TorrentEntry> {
        let info_hash = canonical_info_hash(info_hash);
        let handle = self.by_hash.get(&info_hash)?;
        self.entries.get(handle).map(RegistryRecord::to_entry)
    }

    /// Report whether a torrent is currently held in the compact dormant
    /// representation.  Persistence rollback paths use this to restore the
    /// representation that existed before a failed mutable update; assigning
    /// a cloned entry through `get_mut` would otherwise silently turn every
    /// failed cold-row mutation into a permanently hot allocation.
    pub fn is_dormant(&self, info_hash: &str) -> bool {
        let info_hash = canonical_info_hash(info_hash);
        self.by_hash
            .get(&info_hash)
            .and_then(|handle| self.entries.get(handle))
            .is_some_and(|record| matches!(record, RegistryRecord::Dormant(_)))
    }

    /// Return a deterministic immutable projection for compatibility APIs.
    /// The projection is materialized once per registry revision and then
    /// shared by read-heavy handlers instead of cloning the registry on every
    /// request. Dormant records are expanded only while refreshing this view.
    pub fn snapshot(&self) -> SessionSnapshot {
        let mut cache = self
            .snapshot_cache
            .lock()
            .expect("session snapshot cache mutex poisoned");
        if let Some((revision, entries)) = cache.as_ref() {
            if *revision == self.revision {
                return SessionSnapshot {
                    revision: *revision,
                    entries: Arc::clone(entries),
                };
            }
        }
        let mut entries = self
            .entries
            .values()
            .map(RegistryRecord::to_entry)
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.info_hash.cmp(&right.info_hash));
        let entries = Arc::new(entries);
        *cache = Some((self.revision, Arc::clone(&entries)));
        SessionSnapshot {
            revision: self.revision,
            entries,
        }
    }

    pub fn get_mut(&mut self, info_hash: &str) -> Option<SessionRegistryEntryMut<'_>> {
        let info_hash = canonical_info_hash(info_hash);
        let handle = *self.by_hash.get(&info_hash)?;
        if matches!(self.entries.get(&handle), Some(RegistryRecord::Dormant(_))) {
            let record = self.entries.remove(&handle)?;
            let RegistryRecord::Dormant(dormant) = record else {
                unreachable!("registry record changed while promoting dormant entry")
            };
            self.entries
                .insert(handle, RegistryRecord::Active(dormant.to_entry()));
            self.dormant_count -= 1;
            self.active_count += 1;
        }
        let (entries, stats, revision, changes, change_notify) = (
            &mut self.entries,
            &mut self.stats,
            &mut self.revision,
            &mut self.changes,
            self.change_notify.as_ref(),
        );
        let entry = match entries.get_mut(&handle)? {
            RegistryRecord::Active(entry) => entry,
            RegistryRecord::Dormant(_) => unreachable!("dormant entry was not promoted"),
        };
        let before = EntryContribution::from_entry(entry);
        Some(SessionRegistryEntryMut {
            entry,
            aggregate: stats,
            revision,
            changes,
            change_notify,
            info_hash,
            before,
            touched: false,
        })
    }

    /// Rename a label on active and dormant records without promoting the
    /// dormant records into full runtime entries.  Compatibility category
    /// operations are metadata mutations, not reasons to allocate task-only
    /// state for every stopped torrent.
    pub fn rename_category(&mut self, old_name: &str, new_name: &str) -> usize {
        if old_name == new_name {
            return 0;
        }
        let changed = self
            .entries
            .values_mut()
            .filter_map(|record| {
                let changed = match record {
                    RegistryRecord::Active(entry) => {
                        if entry.category.as_deref() == Some(old_name) {
                            entry.category = Some(new_name.to_owned());
                            true
                        } else {
                            false
                        }
                    }
                    RegistryRecord::Dormant(entry) => {
                        if entry.category.as_deref() == Some(old_name) {
                            entry.category = Some(Arc::<str>::from(new_name));
                            true
                        } else {
                            false
                        }
                    }
                };
                if changed {
                    Some(record.info_hash().to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for info_hash in &changed {
            self.bump_revision(info_hash.clone(), false);
        }
        changed.len()
    }

    /// Clear selected labels while retaining dormant records in their compact
    /// representation.  Returns the number of changed torrent projections.
    pub fn clear_categories(&mut self, names: &[String]) -> usize {
        let names = names
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let changed = self
            .entries
            .values_mut()
            .filter_map(|record| {
                let changed = match record {
                    RegistryRecord::Active(entry) => {
                        if entry
                            .category
                            .as_deref()
                            .is_some_and(|category| names.contains(category))
                        {
                            entry.category = None;
                            true
                        } else {
                            false
                        }
                    }
                    RegistryRecord::Dormant(entry) => {
                        if entry
                            .category
                            .as_deref()
                            .is_some_and(|category| names.contains(category))
                        {
                            entry.category = None;
                            true
                        } else {
                            false
                        }
                    }
                };
                if changed {
                    Some(record.info_hash().to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for info_hash in &changed {
            self.bump_revision(info_hash.clone(), false);
        }
        changed.len()
    }

    /// Clear selected tags on active and dormant records without promoting
    /// dormant records into full runtime entries.  The engine calls this only
    /// after the durable rows have been updated successfully.
    pub fn clear_tags(&mut self, names: &[String]) -> usize {
        let names = names
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let changed = self
            .entries
            .values_mut()
            .filter_map(|record| {
                let changed = match record {
                    RegistryRecord::Active(entry) => {
                        let before = entry.tags.len();
                        entry.tags.retain(|tag| !names.contains(tag.as_str()));
                        before != entry.tags.len()
                    }
                    RegistryRecord::Dormant(entry) => {
                        let before = entry.tags.len();
                        let retained = entry
                            .tags
                            .iter()
                            .filter(|tag| !names.contains(tag.as_str()))
                            .cloned()
                            .collect::<Vec<_>>();
                        let changed = before != retained.len();
                        if changed {
                            entry.tags = Arc::<[String]>::from(retained.into_boxed_slice());
                        }
                        changed
                    }
                };
                if changed {
                    Some(record.info_hash().to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for info_hash in &changed {
            self.bump_revision(info_hash.clone(), false);
        }
        changed.len()
    }

    /// Add peer endpoints to the engine-wide ban set. Peer bans are policy,
    /// not torrent projection data, so they deliberately do not advance the
    /// torrent snapshot revision.
    pub fn ban_peers<I>(&mut self, peers: I) -> usize
    where
        I: IntoIterator<Item = SocketAddr>,
    {
        let accepted = self.bannable_peers(peers);
        let count = accepted.len();
        self.banned_peers.extend(accepted);
        count
    }

    /// Return new peers that fit in the bounded policy set without mutating
    /// it. The engine persists exactly this admission set.
    pub fn bannable_peers<I>(&self, peers: I) -> Vec<SocketAddr>
    where
        I: IntoIterator<Item = SocketAddr>,
    {
        let available = MAX_BANNED_PEERS.saturating_sub(self.banned_peers.len());
        let mut seen = HashSet::with_capacity(available.min(64));
        peers
            .into_iter()
            .filter(|peer| !self.banned_peers.contains(peer) && seen.insert(*peer))
            .take(available)
            .collect()
    }

    pub fn banned_peers(&self) -> Vec<SocketAddr> {
        let mut peers = self.banned_peers.iter().copied().collect::<Vec<_>>();
        peers.sort_unstable();
        peers
    }

    pub fn is_peer_banned(&self, peer: SocketAddr) -> bool {
        self.banned_peers.contains(&peer)
    }

    pub fn banned_peer_count(&self) -> usize {
        self.banned_peers.len()
    }

    /// Return aggregate torrent counters without iterating over the registry.
    pub fn stats(&self) -> SessionRegistryStats {
        self.stats
    }

    /// Current registry generation used by API snapshot/cursor consumers.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Return compact changes after `revision`, or `None` when the requested
    /// cursor has fallen out of the bounded log and the consumer must
    /// resnapshot.
    pub fn changes_since(&self, revision: u64) -> Option<Vec<RegistryChange>> {
        if revision >= self.revision {
            return Some(Vec::new());
        }
        if self
            .changes
            .front()
            .is_some_and(|first| revision.saturating_add(1) < first.revision)
        {
            return None;
        }
        Some(
            self.changes
                .iter()
                .filter(|change| change.revision > revision)
                .cloned()
                .collect(),
        )
    }

    /// Return a notifier that fires whenever the registry revision advances.
    /// API consumers can wait on this instead of polling the registry once per
    /// second while idle. The notifier is owned by the registry so every
    /// mutation path, including engine-owned updates, wakes all consumers.
    pub fn change_notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.change_notify)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn active_len(&self) -> usize {
        self.active_count
    }

    pub fn dormant_len(&self) -> usize {
        self.dormant_count
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = TorrentEntry> + '_ {
        self.entries.values().map(RegistryRecord::to_entry)
    }

    pub fn by_state(&self, state: TorrentState) -> Vec<TorrentEntry> {
        self.entries
            .values()
            .filter(|entry| entry.state() == state)
            .map(RegistryRecord::to_entry)
            .collect()
    }

    fn bump_revision(&mut self, info_hash: String, removed: bool) {
        self.snapshot_cache
            .lock()
            .expect("session snapshot cache mutex poisoned")
            .take();
        self.revision = self.revision.wrapping_add(1);
        self.changes.push_back(RegistryChange {
            revision: self.revision,
            info_hash,
            removed,
        });
        while self.changes.len() > CHANGE_LOG_CAPACITY {
            self.changes.pop_front();
        }
        self.change_notify.notify_waiters();
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrent::TorrentEntry;

    fn entry(hash: &str) -> TorrentEntry {
        TorrentEntry::new(hash.to_owned(), "name".into(), "/data".into())
    }

    #[test]
    fn add_and_get() {
        let mut reg = SessionRegistry::new();
        let e = entry("aaa");
        reg.add(e).unwrap();
        assert!(reg.get("aaa").is_some());
    }

    #[test]
    fn valid_hex_infohashes_are_canonical_and_case_insensitive() {
        let mut reg = SessionRegistry::new();
        let uppercase = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        reg.add(entry(uppercase)).unwrap();

        assert_eq!(
            reg.get(uppercase).unwrap().info_hash,
            uppercase.to_ascii_lowercase()
        );
        assert!(reg.get(&uppercase.to_ascii_lowercase()).is_some());
        assert!(reg.get_mut(&uppercase.to_ascii_lowercase()).is_some());
        assert!(reg.remove(uppercase).is_ok());
    }

    #[test]
    fn peer_bans_are_shared_without_affecting_torrent_counts() {
        let mut reg = SessionRegistry::new();
        reg.add(entry("aaa")).unwrap();
        let peer = "127.0.0.1:6881".parse().unwrap();
        assert_eq!(reg.ban_peers([peer, peer]), 1);
        assert!(reg.is_peer_banned(peer));
        assert_eq!(reg.banned_peer_count(), 1);
        assert_eq!(reg.stats().torrents_total, 1);
    }

    #[test]
    fn duplicate_rejected() {
        let mut reg = SessionRegistry::new();
        reg.add(entry("aaa")).unwrap();
        assert!(matches!(
            reg.add(entry("aaa")),
            Err(SessionError::AlreadyExists(_))
        ));
    }

    #[test]
    fn remove_returns_entry() {
        let mut reg = SessionRegistry::new();
        reg.add(entry("aaa")).unwrap();
        let removed = reg.remove("aaa").unwrap();
        assert_eq!(removed.info_hash, "aaa");
        assert!(reg.get("aaa").is_none());
    }

    #[test]
    fn remove_missing_errors() {
        let mut reg = SessionRegistry::new();
        assert!(matches!(reg.remove("zzz"), Err(SessionError::NotFound(_))));
    }

    #[test]
    fn by_state_filters() {
        let mut reg = SessionRegistry::new();
        let mut e1 = entry("a");
        e1.transition(TorrentState::Checking).unwrap();
        e1.transition(TorrentState::Seeding).unwrap();
        reg.add(e1).unwrap();
        reg.add(entry("b")).unwrap(); // stopped
        let seeding = reg.by_state(TorrentState::Seeding);
        assert_eq!(seeding.len(), 1);
        assert_eq!(seeding[0].info_hash, "a");
    }

    #[test]
    fn len_tracks_count() {
        let mut reg = SessionRegistry::new();
        assert_eq!(reg.len(), 0);
        reg.add(entry("a")).unwrap();
        reg.add(entry("b")).unwrap();
        assert_eq!(reg.len(), 2);
        reg.remove("a").unwrap();
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn revision_changes_on_add_mutate_and_remove() {
        let mut reg = SessionRegistry::new();
        let initial = reg.revision();
        reg.add(entry("a")).unwrap();
        let after_add = reg.revision();
        assert_ne!(after_add, initial);

        reg.get_mut("a").unwrap().name = "changed".to_owned();
        let after_mutate = reg.revision();
        assert_ne!(after_mutate, after_add);

        reg.remove("a").unwrap();
        assert_ne!(reg.revision(), after_mutate);
    }

    #[test]
    fn no_op_mutable_borrow_does_not_advance_revision_or_journal() {
        let mut reg = SessionRegistry::new();
        reg.add(entry("a")).unwrap();
        let revision = reg.revision();
        let journal = reg.changes_since(0).unwrap();
        let _entry = reg.get_mut("a").unwrap();
        drop(_entry);
        assert_eq!(reg.revision(), revision);
        assert_eq!(reg.changes_since(0).unwrap(), journal);
    }

    #[test]
    fn changes_since_reports_mutations_and_removals() {
        let mut reg = SessionRegistry::new();
        let initial = reg.revision();
        reg.add(entry("a")).unwrap();
        reg.get_mut("a").unwrap().name = "changed".to_owned();
        reg.remove("a").unwrap();

        let changes = reg.changes_since(initial).unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].info_hash, "a");
        assert!(!changes[0].removed);
        assert!(changes[2].removed);
    }

    #[test]
    fn aggregate_stats_follow_mutation_and_removal() {
        let mut reg = SessionRegistry::new();
        let mut entry = entry("a");
        entry.total_length = 100;
        entry.amount_left = 60;
        entry.stats.uploaded = 7;
        entry.stats.downloaded = 40;
        reg.add(entry).unwrap();

        let stats = reg.stats();
        assert_eq!(stats.torrents_total, 1);
        assert_eq!(stats.torrents_stopped, 1);
        assert_eq!(stats.bytes_uploaded, 7);
        assert_eq!(stats.bytes_downloaded, 40);
        assert_eq!(stats.bytes_total, 100);
        assert_eq!(stats.bytes_left, 60);

        {
            let mut entry = reg.get_mut("a").unwrap();
            entry.transition(TorrentState::Downloading).unwrap();
            entry.stats.uploaded = 12;
            entry.amount_left = 25;
        }
        let stats = reg.stats();
        assert_eq!(stats.torrents_stopped, 0);
        assert_eq!(stats.torrents_downloading, 1);
        assert_eq!(stats.bytes_uploaded, 12);
        assert_eq!(stats.bytes_total, 100);
        assert_eq!(stats.bytes_left, 25);

        reg.remove("a").unwrap();
        assert_eq!(reg.stats(), SessionRegistryStats::default());
    }

    #[test]
    fn dormant_entries_are_compact_until_mutated() {
        let mut reg = SessionRegistry::new();
        let mut entry = entry("a");
        entry.name = "a torrent".to_owned();
        entry.tags = vec!["tag".to_owned()];
        let dormant = entry.clone().into_dormant();
        reg.add_dormant(dormant).unwrap();

        assert_eq!(reg.dormant_len(), 1);
        assert_eq!(reg.active_len(), 0);
        assert_eq!(reg.get("a").unwrap().name, "a torrent");
        assert_eq!(reg.dormant_len(), 1, "read must not promote a dormant row");

        reg.get_mut("a").unwrap().name = "changed".to_owned();
        assert_eq!(reg.dormant_len(), 0);
        assert_eq!(reg.active_len(), 1);
        assert_eq!(reg.get("a").unwrap().name, "changed");
    }

    #[test]
    fn dormant_entries_preserve_error_and_tracker_messages() {
        let mut reg = SessionRegistry::new();
        let mut entry = entry("a");
        entry.error_message = Some("disk full".to_owned());
        entry.tracker_message = Some("not registered".to_owned());
        reg.add_dormant(entry.into_dormant()).unwrap();

        let projected = reg.get("a").unwrap();
        assert_eq!(projected.error_message.as_deref(), Some("disk full"));
        assert_eq!(projected.tracker_message.as_deref(), Some("not registered"));

        let snapshot = reg.snapshot();
        assert_eq!(
            snapshot.get(0).unwrap().error_message.as_deref(),
            Some("disk full")
        );
        assert_eq!(
            snapshot.get(0).unwrap().tracker_message.as_deref(),
            Some("not registered")
        );
    }
}
