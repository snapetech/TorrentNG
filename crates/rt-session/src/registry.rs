/// In-memory registry of all active torrent sessions.
use std::collections::HashMap;

use crate::{
    error::SessionError,
    state::TorrentState,
    torrent::{TorrentEntry, TorrentHandle},
};

pub struct SessionRegistry {
    /// Keyed by hex infohash for O(1) lookup by hash.
    by_hash: HashMap<String, TorrentHandle>,
    entries: HashMap<TorrentHandle, TorrentEntry>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        SessionRegistry {
            by_hash: HashMap::new(),
            entries: HashMap::new(),
        }
    }

    pub fn add(&mut self, entry: TorrentEntry) -> Result<TorrentHandle, SessionError> {
        if self.by_hash.contains_key(&entry.info_hash) {
            return Err(SessionError::AlreadyExists(entry.info_hash.clone()));
        }
        let handle = entry.handle;
        self.by_hash.insert(entry.info_hash.clone(), handle);
        self.entries.insert(handle, entry);
        Ok(handle)
    }

    pub fn remove(&mut self, info_hash: &str) -> Result<TorrentEntry, SessionError> {
        let handle = self
            .by_hash
            .remove(info_hash)
            .ok_or_else(|| SessionError::NotFound(info_hash.to_owned()))?;
        Ok(self.entries.remove(&handle).unwrap())
    }

    pub fn get(&self, info_hash: &str) -> Option<&TorrentEntry> {
        let handle = self.by_hash.get(info_hash)?;
        self.entries.get(handle)
    }

    pub fn get_mut(&mut self, info_hash: &str) -> Option<&mut TorrentEntry> {
        let handle = self.by_hash.get(info_hash)?;
        self.entries.get_mut(handle)
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
}
