use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
// tokio's Instant, not std's: it respects the paused/mockable clock that
// `#[tokio::test(start_paused = true)]` and `tokio::time::advance()` use.
// Using std::time::Instant here would make the refill calculation below see
// real wall-clock time regardless of virtual time advances, which live-locks
// a paused-clock test into an infinite near-zero-wait retry loop.
use tokio::time::Instant;

const MAX_INITIAL_BURST_BYTES: u64 = 64 * 1024;

/// Process-wide network admission and traffic budgets.
///
/// The peer semaphore counts live peer/metadata connections. The rate
/// limiters count payload bytes at the engine boundary; protocol framing and
/// TCP/IP overhead are not included in the current accounting contract.
#[derive(Clone)]
pub(crate) struct GlobalNetworkBudget {
    peer_slots: Arc<Semaphore>,
    download: Arc<SharedRateLimiter>,
    upload: Arc<SharedRateLimiter>,
}

impl GlobalNetworkBudget {
    pub(crate) fn new(
        max_peers: usize,
        download_limit_bytes_per_sec: Option<u64>,
        upload_limit_bytes_per_sec: Option<u64>,
    ) -> Self {
        Self {
            peer_slots: Arc::new(Semaphore::new(max_peers.max(1))),
            download: Arc::new(SharedRateLimiter::new(download_limit_bytes_per_sec)),
            upload: Arc::new(SharedRateLimiter::new(upload_limit_bytes_per_sec)),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn unlimited() -> Self {
        Self::new(1_000_000, None, None)
    }

    pub(crate) fn try_acquire_peer(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::TryAcquireError> {
        Arc::clone(&self.peer_slots).try_acquire_owned()
    }

    pub(crate) fn download(&self) -> Arc<SharedRateLimiter> {
        Arc::clone(&self.download)
    }

    pub(crate) fn upload(&self) -> Arc<SharedRateLimiter> {
        Arc::clone(&self.upload)
    }

    pub(crate) fn set_download_limit(&self, limit: Option<u64>) {
        self.download.set_limit(limit);
    }

    pub(crate) fn set_upload_limit(&self, limit: Option<u64>) {
        self.upload.set_limit(limit);
    }
}

#[derive(Debug)]
struct RateState {
    limit_bytes_per_sec: Option<u64>,
    tokens: u64,
    updated_at: Instant,
}

/// A small shared token bucket. The mutex is held only for arithmetic; waits
/// happen outside it so a slow torrent cannot block unrelated tasks.
pub(crate) struct SharedRateLimiter {
    state: Mutex<RateState>,
}

impl SharedRateLimiter {
    fn new(limit_bytes_per_sec: Option<u64>) -> Self {
        let capacity = limit_bytes_per_sec
            .filter(|limit| *limit > 0)
            .map(|limit| limit.max(MAX_INITIAL_BURST_BYTES))
            .unwrap_or(u64::MAX);
        Self {
            state: Mutex::new(RateState {
                limit_bytes_per_sec: limit_bytes_per_sec.filter(|limit| *limit > 0),
                tokens: capacity,
                updated_at: Instant::now(),
            }),
        }
    }

    fn set_limit(&self, limit: Option<u64>) {
        let limit = limit.filter(|limit| *limit > 0);
        let capacity = limit
            .map(|limit| limit.max(MAX_INITIAL_BURST_BYTES))
            .unwrap_or(u64::MAX);
        let mut state = self.state.lock().expect("network budget mutex poisoned");
        state.limit_bytes_per_sec = limit;
        state.tokens = capacity;
        state.updated_at = Instant::now();
    }

    pub(crate) async fn acquire(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut remaining = bytes;
        loop {
            let wait = {
                let mut state = self.state.lock().expect("network budget mutex poisoned");
                let Some(limit) = state.limit_bytes_per_sec else {
                    return;
                };
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(state.updated_at);
                state.updated_at = now;
                let refill = ((elapsed.as_nanos() * u128::from(limit)) / 1_000_000_000)
                    .min(u128::from(u64::MAX)) as u64;
                state.tokens = state
                    .tokens
                    .saturating_add(refill)
                    .min(limit.max(MAX_INITIAL_BURST_BYTES));
                // A protocol frame can be larger than the initial burst
                // (for example, a future metadata/data path may account for
                // a whole bounded frame). Consume it in bucket-sized chunks;
                // asking the bucket for more than its capacity would
                // otherwise wait forever because tokens can never reach that
                // request size.
                let requested = remaining.min(limit.max(MAX_INITIAL_BURST_BYTES));
                if state.tokens >= requested {
                    state.tokens -= requested;
                    remaining -= requested;
                    if remaining == 0 {
                        return;
                    }
                    Duration::ZERO
                } else {
                    let missing = requested.saturating_sub(state.tokens);
                    let wait_nanos = ((u128::from(missing) * 1_000_000_000) / u128::from(limit))
                        .min(u128::from(u64::MAX)) as u64;
                    Duration::from_nanos(wait_nanos)
                }
            };
            if wait.is_zero() {
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_budget_does_not_wait() {
        let limiter = SharedRateLimiter::new(None);
        tokio::time::timeout(Duration::from_millis(50), limiter.acquire(1_000_000))
            .await
            .expect("unlimited limiter should not wait");
    }

    #[tokio::test(start_paused = true)]
    async fn limited_budget_refills_after_wait() {
        let limiter = Arc::new(SharedRateLimiter::new(Some(1_000)));
        limiter.acquire(MAX_INITIAL_BURST_BYTES).await;
        let waiter_limiter = Arc::clone(&limiter);
        let waiter = tokio::spawn(async move { waiter_limiter.acquire(1_000).await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        waiter.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn limited_budget_accepts_request_larger_than_bucket_capacity() {
        let limiter = Arc::new(SharedRateLimiter::new(Some(1_000)));
        let waiter_limiter = Arc::clone(&limiter);
        let waiter = tokio::spawn(async move {
            waiter_limiter
                .acquire(MAX_INITIAL_BURST_BYTES.saturating_mul(2))
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_secs(65)).await;
        waiter.await.unwrap();
    }

    #[test]
    fn peer_slots_are_shared_across_clones() {
        let budget = GlobalNetworkBudget::new(1, None, None);
        let clone = budget.clone();
        let permit = budget.try_acquire_peer().unwrap();
        assert!(clone.try_acquire_peer().is_err());
        drop(permit);
        assert!(clone.try_acquire_peer().is_ok());
    }
}
