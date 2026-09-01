/// In-memory registry of all active torrent sessions.
use std::collections::{HashMap, VecDeque};

use crate::{
    error::SessionError,
    state::TorrentState,
    torrent::{TorrentEntry, TorrentHandle},
};

pub struct SessionRegistry {
    /// Keyed by hex infohash for O(1) lookup by hash.
    by_hash: HashMap<String, TorrentHandle>,
    entries: HashMap<TorrentHandle, TorrentEntry>,
    /// Monotonic generation for snapshot consumers.  Mutating access bumps
    /// this before returning a mutable entry; callers therefore cannot
    /// accidentally publish a snapshot that predates an in-flight update.
    revision: u64,
    changes: VecDeque<RegistryChange>,
}

const CHANGE_LOG_CAPACITY: usize = 16_384;

/// A compact mutation record for incremental API/SSE consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryChange {
    pub revision: u64,
    pub info_hash: String,
    pub removed: bool,
}

impl SessionRegistry {
    pub fn new() -> Self {
        SessionRegistry {
            by_hash: HashMap::new(),
            entries: HashMap::new(),
            revision: 0,
            changes: VecDeque::new(),
        }
    }

    pub fn add(&mut self, entry: TorrentEntry) -> Result<TorrentHandle, SessionError> {
        if self.by_hash.contains_key(&entry.info_hash) {
            return Err(SessionError::AlreadyExists(entry.info_hash.clone()));
        }
        let handle = entry.handle;
        let info_hash = entry.info_hash.clone();
        self.by_hash.insert(info_hash.clone(), handle);
        self.entries.insert(handle, entry);
        self.bump_revision(info_hash, false);
        Ok(handle)
    }

    pub fn remove(&mut self, info_hash: &str) -> Result<TorrentEntry, SessionError> {
        let handle = self
            .by_hash
            .remove(info_hash)
            .ok_or_else(|| SessionError::NotFound(info_hash.to_owned()))?;
        let entry = self.entries.remove(&handle).unwrap();
        self.bump_revision(entry.info_hash.clone(), true);
        Ok(entry)
    }

    pub fn get(&self, info_hash: &str) -> Option<&TorrentEntry> {
        let handle = self.by_hash.get(info_hash)?;
        self.entries.get(handle)
    }

    pub fn get_mut(&mut self, info_hash: &str) -> Option<&mut TorrentEntry> {
        let handle = *self.by_hash.get(info_hash)?;
        self.bump_revision(info_hash.to_owned(), false);
        self.entries.get_mut(&handle)
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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TorrentEntry> {
        self.entries.values()
    }

    pub fn by_state(&self, state: TorrentState) -> Vec<&TorrentEntry> {
        self.entries.values().filter(|e| e.state == state).collect()
    }

    fn bump_revision(&mut self, info_hash: String, removed: bool) {
        self.revision = self.revision.wrapping_add(1);
        self.changes.push_back(RegistryChange {
            revision: self.revision,
            info_hash,
            removed,
        });
        while self.changes.len() > CHANGE_LOG_CAPACITY {
            self.changes.pop_front();
        }
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
}
