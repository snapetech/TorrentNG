//! Bounded process-local idempotency state for HTTP mutation retries.
//!
//! The durable engine jobs remain the source of truth for operations that
//! outlive a process. This store covers the other failure window: a client
//! times out after the server committed a small mutation and retries the
//! same request. It coalesces concurrent requests, rejects key reuse with a
//! different request, and bounds both retention and response memory.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tokio::sync::Notify;

pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
pub const MAX_IDEMPOTENCY_ENTRIES: usize = 1_024;
pub const IDEMPOTENCY_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The request/response size limit for the middleware that uses this store.
/// It is deliberately separate from individual endpoint body limits.
pub const MAX_IDEMPOTENCY_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
enum EntryState {
    InFlight(Arc<Notify>),
    Complete(CachedResponse),
}

#[derive(Debug)]
struct Entry {
    fingerprint: [u8; 32],
    created_at: Instant,
    state: EntryState,
}

#[derive(Debug, Clone)]
pub struct IdempotencyStore {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

/// Owns an in-flight claim and releases it if the HTTP request future is
/// cancelled or panics. Without this guard, a client disconnect after the
/// mutation ran could leave a permanent `InFlight` entry that blocks retries
/// until process restart.
pub struct IdempotencyExecutionGuard {
    store: Arc<IdempotencyStore>,
    key: String,
    fingerprint: [u8; 32],
    armed: bool,
}

impl std::fmt::Debug for IdempotencyExecutionGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdempotencyExecutionGuard")
            .field("key", &self.key)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Claim {
    /// The caller owns execution for this key.
    Execute,
    /// Another request is executing this key. The caller should await the
    /// notifier and then claim again to obtain the completed response.
    Wait(Arc<Notify>),
    /// The key was successfully used before; replay this response.
    Replay(CachedResponse),
    /// The key was reused for a different method/path/body.
    Conflict,
    /// The bounded store is saturated by in-flight requests.
    Saturated,
}

impl IdempotencyStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn claim(&self, key: &str, fingerprint: [u8; 32]) -> Claim {
        let mut entries = self.entries.lock().expect("idempotency mutex poisoned");
        let now = Instant::now();
        entries.retain(|_, entry| {
            matches!(&entry.state, EntryState::InFlight(_))
                || now.saturating_duration_since(entry.created_at) < IDEMPOTENCY_TTL
        });

        if let Some(entry) = entries.get(key) {
            if entry.fingerprint != fingerprint {
                return Claim::Conflict;
            }
            return match &entry.state {
                EntryState::InFlight(notify) => Claim::Wait(Arc::clone(notify)),
                EntryState::Complete(response) => Claim::Replay(response.clone()),
            };
        }

        if entries.len() >= MAX_IDEMPOTENCY_ENTRIES {
            return Claim::Saturated;
        }
        entries.insert(
            key.to_owned(),
            Entry {
                fingerprint,
                created_at: now,
                state: EntryState::InFlight(Arc::new(Notify::new())),
            },
        );
        Claim::Execute
    }

    pub fn execution_guard(
        self: &Arc<Self>,
        key: &str,
        fingerprint: [u8; 32],
    ) -> IdempotencyExecutionGuard {
        IdempotencyExecutionGuard {
            store: Arc::clone(self),
            key: key.to_owned(),
            fingerprint,
            armed: true,
        }
    }

    pub fn complete(&self, key: &str, fingerprint: [u8; 32], response: CachedResponse) {
        let notify = {
            let mut entries = self.entries.lock().expect("idempotency mutex poisoned");
            let Some(entry) = entries.get_mut(key) else {
                return;
            };
            if entry.fingerprint != fingerprint {
                return;
            }
            let EntryState::InFlight(notify) = &entry.state else {
                return;
            };
            let notify = Arc::clone(notify);
            entry.created_at = Instant::now();
            entry.state = EntryState::Complete(response);
            notify
        };
        // `notify_one` preserves a permit for a waiter that races completion;
        // `notify_waiters` wakes all requests already waiting on this key.
        notify.notify_one();
        notify.notify_waiters();
    }

    pub fn abandon(&self, key: &str, fingerprint: [u8; 32]) {
        let notify = {
            let mut entries = self.entries.lock().expect("idempotency mutex poisoned");
            let should_remove = entries
                .get(key)
                .is_some_and(|entry| entry.fingerprint == fingerprint);
            if !should_remove {
                return;
            }
            let entry = entries.remove(key).expect("idempotency entry disappeared");
            match entry.state {
                EntryState::InFlight(notify) => Some(notify),
                EntryState::Complete(_) => None,
            }
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

impl IdempotencyExecutionGuard {
    pub fn complete(&mut self, response: CachedResponse) {
        if self.armed {
            self.store.complete(&self.key, self.fingerprint, response);
            self.armed = false;
        }
    }

    pub fn abandon(&mut self) {
        if self.armed {
            self.store.abandon(&self.key, self.fingerprint);
            self.armed = false;
        }
    }
}

impl Drop for IdempotencyExecutionGuard {
    fn drop(&mut self) {
        self.abandon();
    }
}

pub fn request_fingerprint(method: &str, path: &str, body: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update([0]);
    hasher.update(path.as_bytes());
    hasher.update([0]);
    hasher.update(body);
    hasher.finalize().into()
}

pub fn valid_idempotency_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_IDEMPOTENCY_KEY_BYTES
        && key.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(body: &[u8]) -> [u8; 32] {
        request_fingerprint("POST", "/api/v1/test", body)
    }

    #[test]
    fn same_key_replays_and_different_body_conflicts() {
        let store = IdempotencyStore::new();
        let key = "request-1";
        let first = fingerprint(b"one");
        assert!(matches!(store.claim(key, first), Claim::Execute));
        store.complete(
            key,
            first,
            CachedResponse {
                status: 204,
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert!(matches!(store.claim(key, first), Claim::Replay(_)));
        assert!(matches!(
            store.claim(key, fingerprint(b"two")),
            Claim::Conflict
        ));
    }

    #[test]
    fn abandoned_key_can_be_retried() {
        let store = IdempotencyStore::new();
        let key = "request-1";
        let fp = fingerprint(b"one");
        assert!(matches!(store.claim(key, fp), Claim::Execute));
        store.abandon(key, fp);
        assert!(matches!(store.claim(key, fp), Claim::Execute));
    }

    #[test]
    fn dropped_execution_guard_releases_claim_for_retry() {
        let store = IdempotencyStore::new();
        let key = "request-1";
        let fp = fingerprint(b"one");
        assert!(matches!(store.claim(key, fp), Claim::Execute));
        {
            let _guard = store.execution_guard(key, fp);
        }
        assert!(matches!(store.claim(key, fp), Claim::Execute));
    }

    #[test]
    fn keys_are_bounded_to_printable_ascii() {
        assert!(valid_idempotency_key("abc-123"));
        assert!(!valid_idempotency_key(""));
        assert!(!valid_idempotency_key("has space"));
        assert!(!valid_idempotency_key("é"));
        assert!(!valid_idempotency_key(
            &"x".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)
        ));
    }
}
