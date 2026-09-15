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
/// Aggregate bound for retained successful idempotency responses. The entry
/// count alone would permit roughly 8 GiB at the per-response limit.
pub const MAX_IDEMPOTENCY_CACHE_BYTES: usize = 32 * 1024 * 1024;

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

#[derive(Debug)]
struct StoreState {
    entries: HashMap<String, Entry>,
    cached_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct IdempotencyStore {
    state: Arc<Mutex<StoreState>>,
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
            state: Arc::new(Mutex::new(StoreState {
                entries: HashMap::new(),
                cached_bytes: 0,
            })),
        })
    }

    pub fn claim(&self, key: &str, fingerprint: [u8; 32]) -> Claim {
        let mut state = self.state.lock().expect("idempotency mutex poisoned");
        let now = Instant::now();
        {
            let StoreState {
                entries,
                cached_bytes,
            } = &mut *state;
            entries.retain(|_, entry| {
                let keep = matches!(&entry.state, EntryState::InFlight(_))
                    || now.saturating_duration_since(entry.created_at) < IDEMPOTENCY_TTL;
                if !keep {
                    if let EntryState::Complete(response) = &entry.state {
                        *cached_bytes = cached_bytes.saturating_sub(cached_response_size(response));
                    }
                }
                keep
            });
        }

        if let Some(entry) = state.entries.get(key) {
            if entry.fingerprint != fingerprint {
                return Claim::Conflict;
            }
            return match &entry.state {
                EntryState::InFlight(notify) => Claim::Wait(Arc::clone(notify)),
                EntryState::Complete(response) => Claim::Replay(response.clone()),
            };
        }

        if state.entries.len() >= MAX_IDEMPOTENCY_ENTRIES {
            return Claim::Saturated;
        }
        state.entries.insert(
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

    pub fn complete(&self, key: &str, fingerprint: [u8; 32], response: CachedResponse) -> bool {
        let response_bytes = cached_response_size(&response);
        let (notify, cached) = {
            let mut state = self.state.lock().expect("idempotency mutex poisoned");
            let Some(entry) = state.entries.get(key) else {
                return false;
            };
            if entry.fingerprint != fingerprint {
                return false;
            }
            let EntryState::InFlight(notify) = &entry.state else {
                return false;
            };
            let notify = Arc::clone(notify);

            while state.cached_bytes.saturating_add(response_bytes) > MAX_IDEMPOTENCY_CACHE_BYTES {
                let oldest_key = state
                    .entries
                    .iter()
                    .filter_map(|(entry_key, entry)| match &entry.state {
                        EntryState::Complete(_) => Some((entry_key, entry.created_at)),
                        EntryState::InFlight(_) => None,
                    })
                    .min_by_key(|(_, created_at)| *created_at)
                    .map(|(entry_key, _)| entry_key.clone());
                let Some(oldest_key) = oldest_key else {
                    break;
                };
                if let Some(removed) = state.entries.remove(&oldest_key) {
                    if let EntryState::Complete(response) = removed.state {
                        state.cached_bytes = state
                            .cached_bytes
                            .saturating_sub(cached_response_size(&response));
                    }
                }
            }

            if state.cached_bytes.saturating_add(response_bytes) > MAX_IDEMPOTENCY_CACHE_BYTES {
                // A response larger than the aggregate budget cannot be
                // retained. Release waiters instead of leaving an in-flight
                // claim that can never transition to a replayable result.
                let removed = state.entries.remove(key);
                let notify = match removed.map(|entry| entry.state) {
                    Some(EntryState::InFlight(notify)) => notify,
                    _ => return false,
                };
                (notify, false)
            } else {
                let Some(entry) = state.entries.get_mut(key) else {
                    return false;
                };
                entry.created_at = Instant::now();
                entry.state = EntryState::Complete(response);
                state.cached_bytes = state.cached_bytes.saturating_add(response_bytes);
                (notify, true)
            }
        };
        // `notify_one` preserves a permit for a waiter that races completion;
        // `notify_waiters` wakes all requests already waiting on this key.
        notify.notify_one();
        notify.notify_waiters();
        cached
    }

    pub fn abandon(&self, key: &str, fingerprint: [u8; 32]) {
        let notify = {
            let mut state = self.state.lock().expect("idempotency mutex poisoned");
            let should_remove = state
                .entries
                .get(key)
                .is_some_and(|entry| entry.fingerprint == fingerprint);
            if !should_remove {
                return;
            }
            let entry = state
                .entries
                .remove(key)
                .expect("idempotency entry disappeared");
            match entry.state {
                EntryState::InFlight(notify) => Some(notify),
                EntryState::Complete(response) => {
                    state.cached_bytes = state
                        .cached_bytes
                        .saturating_sub(cached_response_size(&response));
                    None
                }
            }
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

fn cached_response_size(response: &CachedResponse) -> usize {
    response.body.len().saturating_add(
        response
            .headers
            .iter()
            .map(|(name, value)| name.len().saturating_add(value.len()))
            .fold(0usize, usize::saturating_add),
    )
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

    #[test]
    fn completed_responses_are_evicted_at_the_aggregate_byte_bound() {
        let store = IdempotencyStore::new();
        let body_size = MAX_IDEMPOTENCY_CACHE_BYTES / 2;
        let keys = ["cache-1", "cache-2", "cache-3"];
        for key in keys {
            let fp = fingerprint(key.as_bytes());
            assert!(matches!(store.claim(key, fp), Claim::Execute));
            assert!(store.complete(
                key,
                fp,
                CachedResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: vec![0; body_size],
                },
            ));
        }

        let first = fingerprint(b"cache-1");
        assert!(matches!(store.claim("cache-1", first), Claim::Execute));
        let second = fingerprint(b"cache-2");
        assert!(matches!(store.claim("cache-2", second), Claim::Replay(_)));
        let third = fingerprint(b"cache-3");
        assert!(matches!(store.claim("cache-3", third), Claim::Replay(_)));
    }
}
