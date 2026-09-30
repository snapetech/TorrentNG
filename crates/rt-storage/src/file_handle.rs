//! Descriptor leases shared by the path-backed scheduler and its disk jobs.

use std::fs::File;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::fd_limit::process_handle_cache_capacity;
use once_cell::sync::Lazy;
use tokio::sync::Notify;

static PROCESS_DESCRIPTOR_GATE: Lazy<Arc<DescriptorGate>> =
    Lazy::new(|| Arc::new(DescriptorGate::new(process_handle_cache_capacity().max(1))));
static DESCRIPTOR_SIGNAL: Lazy<Arc<DescriptorSignal>> =
    Lazy::new(|| Arc::new(DescriptorSignal::default()));
static PROCESS_DESCRIPTOR_BUDGET_WAITS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) struct DescriptorLimiter {
    limit: usize,
    state: Mutex<DescriptorState>,
    process: Arc<DescriptorGate>,
    signal: Arc<DescriptorSignal>,
}

#[derive(Debug, Default)]
struct DescriptorState {
    active: usize,
}

#[derive(Debug)]
struct DescriptorGate {
    limit: usize,
    state: Mutex<DescriptorState>,
}

impl DescriptorGate {
    fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            state: Mutex::new(DescriptorState::default()),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, DescriptorState> {
        lock_recover(&self.state)
    }
}

#[derive(Debug, Default)]
struct DescriptorSignalState {
    generation: u64,
}

#[derive(Debug, Default)]
struct DescriptorSignal {
    state: Mutex<DescriptorSignalState>,
    available: Condvar,
    async_available: Notify,
}

impl DescriptorSignal {
    fn generation(&self) -> u64 {
        lock_recover(&self.state).generation
    }

    fn wait_for_change(&self, generation: u64) {
        let mut state = lock_recover(&self.state);
        while state.generation == generation {
            state = match self.available.wait(state) {
                Ok(state) => state,
                Err(poisoned) => {
                    let state = poisoned.into_inner();
                    self.state.clear_poison();
                    state
                }
            };
        }
    }

    async fn wait_for_change_async(&self, generation: u64) {
        let notified = self.async_available.notified();
        tokio::pin!(notified);
        // Register before checking the generation so a drop cannot be lost
        // between the check and awaiting this notification.
        notified.as_mut().enable();
        if self.generation() != generation {
            return;
        }
        notified.await;
    }

    fn notify_waiters(&self) {
        let mut state = lock_recover(&self.state);
        state.generation = state.generation.wrapping_add(1);
        self.available.notify_all();
        self.async_available.notify_waiters();
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let state = poisoned.into_inner();
            mutex.clear_poison();
            state
        }
    }
}

impl DescriptorLimiter {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Self::with_gates(
            // Even a zero-entry cache must allow one operation to open a file
            // without retaining it.
            limit.max(1),
            Arc::clone(&PROCESS_DESCRIPTOR_GATE),
            Arc::clone(&DESCRIPTOR_SIGNAL),
        )
    }

    fn with_gates(
        limit: usize,
        process: Arc<DescriptorGate>,
        signal: Arc<DescriptorSignal>,
    ) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            state: Mutex::new(DescriptorState::default()),
            process,
            signal,
        })
    }

    #[cfg(test)]
    fn isolated_pair(local_limit: usize, process_limit: usize) -> (Arc<Self>, Arc<Self>) {
        let process = Arc::new(DescriptorGate::new(process_limit));
        let signal = Arc::new(DescriptorSignal::default());
        (
            Self::with_gates(local_limit, Arc::clone(&process), Arc::clone(&signal)),
            Self::with_gates(local_limit, process, signal),
        )
    }

    pub(crate) fn try_acquire(self: &Arc<Self>) -> Option<DescriptorPermit> {
        // Every cache takes the process gate before its local gate. This
        // reserves both budgets atomically with respect to other admissions.
        let mut process = self.process.lock_state();
        if process.active >= self.process.limit {
            PROCESS_DESCRIPTOR_BUDGET_WAITS.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut state = self.lock_state();
        if state.active >= self.limit {
            return None;
        }
        process.active += 1;
        state.active += 1;
        drop(state);
        drop(process);
        Some(DescriptorPermit {
            limiter: Arc::clone(self),
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.signal.generation()
    }

    pub(crate) fn wait_for_change(&self, generation: u64) {
        self.signal.wait_for_change(generation);
    }

    pub(crate) async fn wait_for_change_async(&self, generation: u64) {
        self.signal.wait_for_change_async(generation).await;
    }

    pub(crate) fn active(&self) -> usize {
        self.lock_state().active
    }

    fn notify_waiters(&self) {
        self.signal.notify_waiters();
    }

    fn lock_state(&self) -> MutexGuard<'_, DescriptorState> {
        lock_recover(&self.state)
    }
}

pub(crate) fn process_descriptor_budget_stats() -> (usize, usize, u64) {
    let active = PROCESS_DESCRIPTOR_GATE.lock_state().active;
    (
        active,
        PROCESS_DESCRIPTOR_GATE.limit,
        PROCESS_DESCRIPTOR_BUDGET_WAITS.load(Ordering::Relaxed),
    )
}

#[derive(Debug)]
pub(crate) struct DescriptorPermit {
    limiter: Arc<DescriptorLimiter>,
}

impl Drop for DescriptorPermit {
    fn drop(&mut self) {
        {
            let mut process = self.limiter.process.lock_state();
            let mut local = self.limiter.lock_state();
            debug_assert!(
                process.active > 0 && local.active > 0,
                "descriptor permit released more than once"
            );
            process.active = process.active.saturating_sub(1);
            local.active = local.active.saturating_sub(1);
        }
        self.limiter.signal.notify_waiters();
    }
}

/// An open file whose descriptor slot remains reserved for the complete
/// lifetime of cache, caller, and backend-queue references.
#[derive(Debug)]
pub(crate) struct LeasedFile {
    // Keep the actual descriptor alive until before the permit is released.
    file: File,
    _permit: DescriptorPermit,
}

impl LeasedFile {
    pub(crate) fn new(file: File, permit: DescriptorPermit) -> Self {
        Self {
            file,
            _permit: permit,
        }
    }

    pub(crate) fn notify_waiters(&self) {
        self._permit.limiter.notify_waiters();
    }
}

impl Deref for LeasedFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

/// A caller/backend reference to a cached file. Dropping the final operation
/// reference wakes descriptor waiters even when the cache still owns the
/// underlying descriptor, allowing them to evict that now-idle entry.
#[derive(Debug)]
pub(crate) struct LeasedFileHandle {
    file: Arc<LeasedFile>,
}

impl LeasedFileHandle {
    pub(crate) fn new(file: Arc<LeasedFile>) -> Self {
        Self { file }
    }

    pub(crate) fn as_file(&self) -> &File {
        &self.file
    }
}

impl Deref for LeasedFileHandle {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl Drop for LeasedFileHandle {
    fn drop(&mut self) {
        self.file.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn independent_cache_limiters_share_budget_and_wake_each_others_waiters() {
        let (first, second) = DescriptorLimiter::isolated_pair(8, 1);
        let first_permit = first.try_acquire().expect("first cache uses shared slot");
        assert!(second.try_acquire().is_none());

        let second_waiter = Arc::clone(&second);
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let generation = second_waiter.generation();
            assert!(second_waiter.try_acquire().is_none());
            attempted_tx.send(()).unwrap();
            second_waiter.wait_for_change(generation);
            second_waiter
                .try_acquire()
                .expect("released slot becomes available to another cache")
        });

        attempted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second cache reached descriptor backpressure");
        drop(first_permit);
        let second_permit = waiter.join().expect("descriptor waiter completes");
        assert_eq!(second.active(), 1);
        drop(second_permit);
    }

    #[tokio::test]
    async fn shared_budget_release_wakes_async_waiter_in_another_cache() {
        let (first, second) = DescriptorLimiter::isolated_pair(1, 1);
        let first_permit = first.try_acquire().expect("first cache uses shared slot");
        let second_waiter = Arc::clone(&second);
        let waiter = tokio::spawn(async move {
            loop {
                let generation = second_waiter.generation();
                if let Some(permit) = second_waiter.try_acquire() {
                    return permit;
                }
                second_waiter.wait_for_change_async(generation).await;
            }
        });

        tokio::task::yield_now().await;
        drop(first_permit);
        let second_permit = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("cross-cache async waiter wakes")
            .expect("cross-cache async waiter completes");
        assert_eq!(second.active(), 1);
        drop(second_permit);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_budget_caps_independent_caches_under_low_rlimit() {
        const CHILD_ENV: &str = "TNG_TEST_PROCESS_DESCRIPTOR_BUDGET_LOW_FD_CHILD";

        if std::env::var_os(CHILD_ENV).is_some() {
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `current` is a valid writable `rlimit` for getrlimit.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) },
                0
            );
            let limit = current.rlim_max.clamp(1, 64);
            let constrained = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            // SAFETY: lowering this child process's limits cannot affect the
            // parent test process.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &constrained) },
                0
            );

            let cache_capacity = process_handle_cache_capacity();
            let budget_capacity = cache_capacity.max(1);
            let first_cache = DescriptorLimiter::new(cache_capacity);
            let second_cache = DescriptorLimiter::new(cache_capacity);
            let mut first_permits = Vec::new();
            let mut second_permits = Vec::new();
            for _ in 0..budget_capacity / 2 {
                first_permits.push(first_cache.try_acquire().expect("first cache slot"));
            }
            for _ in budget_capacity / 2..budget_capacity {
                second_permits.push(second_cache.try_acquire().expect("second cache slot"));
            }

            let stats = process_descriptor_budget_stats();
            assert_eq!(stats.0, budget_capacity);
            assert_eq!(stats.1, budget_capacity);
            assert!(first_cache.try_acquire().is_none());
            assert!(second_cache.try_acquire().is_none());

            drop(second_permits.pop().expect("release a second-cache slot"));
            let replacement = first_cache
                .try_acquire()
                .expect("released process slot is shared with the first cache");
            assert_eq!(process_descriptor_budget_stats().0, budget_capacity);

            drop(replacement);
            drop(first_permits);
            drop(second_permits);
            assert_eq!(process_descriptor_budget_stats().0, 0);
            return;
        }

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_handle::tests::process_budget_caps_independent_caches_under_low_rlimit",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "low-fd process-budget child timed out"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let transcript = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success() && transcript.contains("1 passed"),
            "low-fd process-budget child failed or did not run the test:\n{transcript}"
        );
    }
}
