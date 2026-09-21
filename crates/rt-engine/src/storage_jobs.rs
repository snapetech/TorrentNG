//! Bounded, durable storage-plan execution outside the engine actor.
//!
//! The engine actor owns orchestration and command ordering. It must not own
//! filesystem copies, renames, deletes, or long-running verification. This
//! module is the narrow worker seam: plans are queued with a bounded channel,
//! executed on blocking threads behind a fixed semaphore, and checkpointed in
//! the same SQLite database file used by the actor. Production workers use a
//! dedicated SQLite connection so their checkpoints do not serialize behind
//! the actor's connection mutex.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use tokio::sync::{mpsc, oneshot, Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

use rt_storage::{StorageError, StoragePlan, StoragePlanStep};

use crate::engine::unix_now_i64;

const DEFAULT_QUEUE_CAPACITY: usize = 32;
const DEFAULT_WORKER_COUNT: usize = 2;
const STORAGE_WORKER_ABORT_GRACE: Duration = Duration::from_millis(100);
pub(crate) const STORAGE_JOB_STATE_COMMIT_PENDING: &str = "commit_pending";
/// Internal operation whose filesystem completion still needs the engine to
/// delete the torrent's durable metadata projection.
pub(crate) const STORAGE_JOB_OPERATION_PAYLOAD_DELETE: &str = "delete_payload";
pub(crate) const STORAGE_MANUAL_RECOVERY_PREFIX: &str = "manual recovery required: ";

pub(crate) fn manual_recovery_reason(error: impl Into<String>) -> String {
    let error = error.into();
    if error.starts_with(STORAGE_MANUAL_RECOVERY_PREFIX) {
        error
    } else {
        format!("{STORAGE_MANUAL_RECOVERY_PREFIX}{error}")
    }
}

fn db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn db_i64_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn completed_byte_offset(plan: &StoragePlan, completed_steps: &[usize]) -> i64 {
    completed_steps
        .iter()
        .filter_map(|index| plan.steps.get(*index))
        .map(|step| db_i64(step.bytes))
        .fold(0_i64, i64::saturating_add)
}

fn storage_plan_filesystem_complete(plan: &StoragePlan, completed_steps: &[usize]) -> bool {
    if completed_steps.len() != plan.steps.len() {
        return false;
    }
    let completed = completed_steps
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    completed.len() == plan.steps.len()
        && (0..plan.steps.len()).all(|index| completed.contains(&index))
}

fn storage_plan_has_destructive_progress(plan: &StoragePlan, completed_steps: &[usize]) -> bool {
    completed_steps.iter().any(|index| {
        plan.steps.get(*index).is_some_and(|step| {
            matches!(
                step.action,
                rt_storage::PlannedStorageAction::Rename
                    | rt_storage::PlannedStorageAction::SafeDelete
                    | rt_storage::PlannedStorageAction::SafeDeleteIfPresent
                    | rt_storage::PlannedStorageAction::PruneEmptyDirs
            )
        })
    })
}

fn storage_plan_failure_requires_manual_recovery(
    plan: &StoragePlan,
    completed_steps: &[usize],
    error: Option<&StorageError>,
) -> bool {
    error.is_some_and(|error| {
        error.requires_manual_recovery()
            || matches!(
                error,
                StorageError::StagedMoveFailed {
                    step: "checkpoint",
                    ..
                }
            )
    }) || storage_plan_has_destructive_progress(plan, completed_steps)
}

fn is_storage_cancellation_error(error: &StorageError) -> bool {
    matches!(
        error,
        StorageError::Cancelled | StorageError::StagedMoveFailed { step: "cancel", .. }
    )
}

fn storage_job_was_cancelled(
    control_cancelled: bool,
    result: &Result<rt_storage::StoragePlanExecution, StorageError>,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> bool {
    control_cancelled
        && result.as_ref().err().is_some_and(|error| {
            // A pause is reported by the same cooperative step-boundary
            // error path as cancellation. If cancellation arrives after
            // that boundary was observed but before the worker classifies
            // its result, the terminal user request must win rather than
            // stranding the job in `paused`.
            is_storage_cancellation_error(error)
                || matches!(error, StorageError::StagedMoveFailed { step: "pause", .. })
        })
        && !storage_plan_has_destructive_progress(plan, completed_steps)
}

fn cancellation_cleanup_failure(
    plan: &StoragePlan,
    roots: &[std::path::PathBuf],
    completed_steps: &[usize],
) -> Option<String> {
    if completed_steps.is_empty()
        || storage_plan_filesystem_complete(plan, completed_steps)
        || storage_plan_has_destructive_progress(plan, completed_steps)
    {
        return None;
    }
    match rt_storage::rollback_storage_plan_under_roots(plan, roots) {
        Ok(execution) if execution.rollback_failures.is_empty() => None,
        Ok(execution) => Some(format!(
            "{} storage-plan rollback step(s) failed",
            execution.rollback_failures.len()
        )),
        Err(error) => Some(error.to_string()),
    }
}

fn storage_job_terminal_state(
    operation: &str,
    filesystem_complete: bool,
    shutdown_requested: bool,
    cancelled: bool,
    paused: bool,
    execution_succeeded: bool,
) -> &'static str {
    if shutdown_requested {
        "queued"
    } else if filesystem_complete {
        if matches!(operation, "move" | STORAGE_JOB_OPERATION_PAYLOAD_DELETE) {
            // The filesystem transaction is committed, but the actor still
            // has to publish the move's new save path or delete the payload's
            // torrent metadata projection.
            STORAGE_JOB_STATE_COMMIT_PENDING
        } else {
            "completed"
        }
    } else if cancelled {
        "cancelled"
    } else if paused {
        "paused"
    } else if execution_succeeded {
        STORAGE_JOB_STATE_COMMIT_PENDING
    } else {
        "failed"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageJobAction {
    Pause,
    Resume,
    Cancel,
    Shutdown,
}

struct StorageJobControl {
    paused: AtomicBool,
    cancelled: AtomicBool,
    shutdown: AtomicBool,
    fault_delay_ms: u64,
    fault_delay_consumed: AtomicBool,
    notify: Notify,
}

fn lock_storage_job_controls(
    controls: &Mutex<HashMap<String, Arc<StorageJobControl>>>,
) -> MutexGuard<'_, HashMap<String, Arc<StorageJobControl>>> {
    match controls.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // This map only stores owned IDs and atomic control handles; no
            // user code runs while it is locked. Recovering it keeps a prior
            // worker panic from taking down the supervisor during cleanup.
            warn!(
                component = "storage_jobs",
                operation = "controls_lock",
                result = "recovered_poison",
                "recovering storage job controls after mutex poisoning"
            );
            let guard = poisoned.into_inner();
            controls.clear_poison();
            guard
        }
    }
}

fn lock_storage_job_db(db: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>, String> {
    db.lock()
        .map_err(|_| "database mutex poisoned during storage job persistence".to_owned())
}

impl StorageJobControl {
    fn new(paused: bool) -> Self {
        let fault_delay_ms = std::env::var("TNG_FAULT_STORAGE_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Self {
            paused: AtomicBool::new(paused),
            cancelled: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            fault_delay_ms,
            fault_delay_consumed: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn apply(self: &Arc<Self>, action: StorageJobAction) {
        match action {
            StorageJobAction::Pause => self.paused.store(true, Ordering::Release),
            StorageJobAction::Resume => self.paused.store(false, Ordering::Release),
            StorageJobAction::Cancel => self.cancelled.store(true, Ordering::Release),
            StorageJobAction::Shutdown => {
                // Shutdown interrupts at a step boundary, but is not a user
                // cancellation. The durable job must remain recoverable on
                // the next process start.
                self.shutdown.store(true, Ordering::Release);
            }
        }
        self.notify.notify_waiters();
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    async fn wait_until_runnable(&self) {
        loop {
            if !self.is_paused() || self.cancelled.load(Ordering::Acquire) || self.is_shutdown() {
                return;
            }
            let mut notified = std::pin::pin!(self.notify.notified());
            notified.as_mut().enable();
            if !self.is_paused() || self.cancelled.load(Ordering::Acquire) || self.is_shutdown() {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_fault_delay(&self) {
        if self.fault_delay_ms > 0 && !self.fault_delay_consumed.swap(true, Ordering::AcqRel) {
            // Fault injection must not consume a blocking filesystem worker.
            tokio::time::sleep(Duration::from_millis(self.fault_delay_ms)).await;
        }
    }

    /// Check control state from a blocking filesystem worker. A pause is not
    /// waited out on the worker: doing that would consume a worker slot for
    /// the entire pause and starve unrelated storage jobs. The supervisor
    /// persists the completed step, releases the slot, and waits
    /// asynchronously before reattaching this job on resume.
    fn check_step_boundary(&self) -> Result<(), StorageError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
        }
        if self.is_shutdown() {
            return Err(shutdown_error());
        }
        if self.paused.load(Ordering::Acquire) {
            return Err(paused_error());
        }
        Ok(())
    }
}

struct StorageJobRequest {
    job_id: String,
    operation: String,
    plan: StoragePlan,
    completed_steps: Vec<usize>,
    roots: Vec<std::path::PathBuf>,
    db: Arc<Mutex<Connection>>,
    control: Arc<StorageJobControl>,
    completion: oneshot::Sender<StorageJobCompletion>,
}

struct StorageJobExecution {
    job_id: String,
    operation: String,
    plan: StoragePlan,
    completed_steps: Vec<usize>,
    roots: Vec<std::path::PathBuf>,
    db: Arc<Mutex<Connection>>,
    control: Arc<StorageJobControl>,
}

type StorageJobExecutor = Arc<dyn Fn(StorageJobExecution) -> StorageJobCompletion + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StorageJobCompletion {
    pub(crate) succeeded: bool,
    pub(crate) state: String,
    pub(crate) error: Option<String>,
    pub(crate) completed_steps: Vec<usize>,
    /// Progress computed from the same plan instance that performed the
    /// filesystem work. This is needed when terminal persistence fails after
    /// the filesystem has committed and the engine has to finish the job.
    pub(crate) completed_byte_offset: Option<i64>,
    pub(crate) requires_manual_recovery: bool,
}

impl StorageJobCompletion {
    fn terminal(
        state: impl Into<String>,
        error: Option<String>,
        completed_steps: Vec<usize>,
    ) -> Self {
        let state = state.into();
        Self {
            succeeded: matches!(state.as_str(), STORAGE_JOB_STATE_COMMIT_PENDING)
                || (state == "completed" && error.is_none()),
            state,
            error,
            completed_steps,
            completed_byte_offset: None,
            requires_manual_recovery: false,
        }
    }

    pub(crate) fn failed(error: impl Into<String>, completed_steps: Vec<usize>) -> Self {
        Self::terminal("failed", Some(error.into()), completed_steps)
    }

    pub(crate) fn failed_with_manual_recovery(
        error: impl Into<String>,
        completed_steps: Vec<usize>,
    ) -> Self {
        let mut completion = Self::failed(manual_recovery_reason(error), completed_steps);
        completion.requires_manual_recovery = true;
        completion
    }

    fn requeued(error: impl Into<String>, completed_steps: Vec<usize>) -> Self {
        Self::terminal("queued", Some(error.into()), completed_steps)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StorageJobStats {
    pub(crate) queue_depth: usize,
    pub(crate) inflight: usize,
    pub(crate) capacity: usize,
    pub(crate) worker_count: usize,
}

/// Handle for the bounded storage worker supervisor.
pub(crate) struct StorageJobDispatcher {
    tx: mpsc::Sender<StorageJobRequest>,
    controls: Arc<Mutex<HashMap<String, Arc<StorageJobControl>>>>,
    /// Production workers use a separate SQLite connection so checkpoint and
    /// terminal writes cannot make the engine actor wait on its connection's
    /// mutex. Test-only dispatchers leave this unset and use the connection
    /// supplied to `submit`, which also supports in-memory SQLite fixtures.
    worker_db: Option<Arc<Mutex<Connection>>>,
    max_inflight: usize,
    worker_count: usize,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

/// Removes a request's admission slot even when the request future itself
/// panics. The supervisor normally reaps completed futures and performs this
/// cleanup, but a panic is a separate failure path; without the guard a
/// single worker panic would permanently consume one end-to-end capacity
/// slot until restart.
struct StorageJobRegistration {
    job_id: String,
    controls: Arc<Mutex<HashMap<String, Arc<StorageJobControl>>>>,
}

impl Drop for StorageJobRegistration {
    fn drop(&mut self) {
        lock_storage_job_controls(&self.controls).remove(&self.job_id);
    }
}

impl StorageJobDispatcher {
    pub(crate) fn new(db_path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(db_path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        // The engine owns a separate connection. WAL avoids reader/writer
        // serialization, while a finite busy timeout keeps a short SQLite
        // commit collision on the worker thread instead of surfacing a
        // transient SQLITE_BUSY failure to the durable job protocol.
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self::with_worker_db(
            Some(Arc::new(Mutex::new(connection))),
            DEFAULT_WORKER_COUNT,
            DEFAULT_QUEUE_CAPACITY,
        ))
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            controls: Arc::new(Mutex::new(HashMap::new())),
            worker_db: None,
            max_inflight: 2,
            worker_count: 1,
            shutdown_tx: None,
            join: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_limits(
        db: Arc<Mutex<Connection>>,
        worker_count: usize,
        queue_capacity: usize,
    ) -> Self {
        let _ = db;
        Self::with_worker_db(None, worker_count, queue_capacity)
    }

    fn with_worker_db(
        worker_db: Option<Arc<Mutex<Connection>>>,
        worker_count: usize,
        queue_capacity: usize,
    ) -> Self {
        Self::with_worker_db_and_executor(
            worker_db,
            worker_count,
            queue_capacity,
            Arc::new(execute_storage_job),
        )
    }

    #[cfg(test)]
    fn with_test_executor(
        worker_count: usize,
        queue_capacity: usize,
        executor: StorageJobExecutor,
    ) -> Self {
        Self::with_worker_db_and_executor(None, worker_count, queue_capacity, executor)
    }

    fn with_worker_db_and_executor(
        worker_db: Option<Arc<Mutex<Connection>>>,
        worker_count: usize,
        queue_capacity: usize,
        executor: StorageJobExecutor,
    ) -> Self {
        let worker_count = worker_count.max(1);
        let queue_capacity = queue_capacity.max(1);
        let max_inflight = queue_capacity.saturating_add(worker_count);
        // Size the channel to the full end-to-end inflight bound, not just
        // queue_capacity: submit_inner's `controls.len() >= max_inflight`
        // check is what's meant to reject over-capacity submissions (with a
        // clear "dispatcher is at capacity" error). A smaller channel buffer
        // makes `try_send` hit its own, stricter "queue is full" rejection
        // first whenever the supervisor hasn't drained a slot yet, which is
        // a scheduling artifact, not the intended bound.
        let (tx, rx) = mpsc::channel(max_inflight);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let controls = Arc::new(Mutex::new(HashMap::new()));
        let slots = Arc::new(Semaphore::new(worker_count));
        let supervisor_controls = Arc::clone(&controls);
        let join = tokio::spawn(run_supervisor(
            rx,
            shutdown_rx,
            slots,
            supervisor_controls,
            Arc::clone(&executor),
        ));
        Self {
            tx,
            controls,
            worker_db,
            max_inflight,
            worker_count,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }

    // Test-only adapter for in-memory SQLite fixtures. Production callers use
    // the managed variants below so the engine actor never owns the worker DB
    // handle.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit(
        &self,
        db: Arc<Mutex<Connection>>,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            false,
            false,
        )
    }

    /// Reattach a durable paused job without briefly marking it running or
    /// consuming a worker slot. Resume/cancel wakes the supervisor task.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_paused(
        &self,
        db: Arc<Mutex<Connection>>,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            true,
            false,
        )
    }

    /// Reattach a storage job whose cancellation intent was durable before a
    /// process stopped. The request is admitted with cancellation already
    /// set, so the worker performs its normal rollback/cleanup path without
    /// executing another filesystem step.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_cancelled(
        &self,
        db: Arc<Mutex<Connection>>,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            false,
            true,
        )
    }

    /// Submit using the connection owned by the production storage
    /// supervisor. The engine actor does not need to retain or pass its
    /// startup connection into this path.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_managed(
        &self,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        let db = self
            .worker_db
            .as_ref()
            .cloned()
            .ok_or_else(|| "storage worker database is unavailable".to_owned())?;
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            false,
            false,
        )
    }

    /// Reattach a durably cancelling job with cancellation already set on
    /// its control. Recovery can therefore finish cleanup after a crash
    /// without briefly resuming the storage plan.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_cancelled_managed(
        &self,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        let db = self
            .worker_db
            .as_ref()
            .cloned()
            .ok_or_else(|| "storage worker database is unavailable".to_owned())?;
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            false,
            true,
        )
    }

    /// Reattach a paused job using the production supervisor's private
    /// connection.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn submit_paused_managed(
        &self,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
    ) -> Result<(), String> {
        let db = self
            .worker_db
            .as_ref()
            .cloned()
            .ok_or_else(|| "storage worker database is unavailable".to_owned())?;
        self.submit_inner(
            db,
            job_id,
            operation,
            plan,
            completed_steps,
            roots,
            completion,
            true,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_inner(
        &self,
        db: Arc<Mutex<Connection>>,
        job_id: String,
        operation: String,
        plan: StoragePlan,
        completed_steps: Vec<usize>,
        roots: Vec<std::path::PathBuf>,
        completion: oneshot::Sender<StorageJobCompletion>,
        paused: bool,
        cancelled: bool,
    ) -> Result<(), String> {
        let db = self.worker_db.as_ref().cloned().unwrap_or(db);
        let control = Arc::new(StorageJobControl::new(paused));
        if cancelled {
            control.apply(StorageJobAction::Cancel);
        }
        {
            let mut controls = lock_storage_job_controls(&self.controls);
            if controls.len() >= self.max_inflight {
                return Err("storage job dispatcher is at capacity".to_owned());
            }
            if controls.contains_key(&job_id) {
                return Err(format!("storage job {job_id} is already active"));
            }
            controls.insert(job_id.clone(), Arc::clone(&control));
        }
        let request = StorageJobRequest {
            job_id: job_id.clone(),
            operation,
            plan,
            completed_steps,
            roots,
            db,
            control,
            completion,
        };
        if let Err(error) = self.tx.try_send(request) {
            lock_storage_job_controls(&self.controls).remove(&job_id);
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => "storage job worker queue is full".to_owned(),
                mpsc::error::TrySendError::Closed(_) => {
                    "storage job worker is shutting down".to_owned()
                }
            });
        }
        Ok(())
    }

    pub(crate) fn control(&self, job_id: &str, action: StorageJobAction) -> Result<(), String> {
        let control = lock_storage_job_controls(&self.controls)
            .get(job_id)
            .cloned()
            .ok_or_else(|| format!("storage worker has no active job {job_id}"))?;
        control.apply(action);
        Ok(())
    }

    pub(crate) fn stats(&self) -> StorageJobStats {
        let inflight = lock_storage_job_controls(&self.controls).len();
        StorageJobStats {
            queue_depth: self.max_inflight.saturating_sub(self.tx.capacity()),
            inflight,
            capacity: self.max_inflight,
            worker_count: self.worker_count,
        }
    }

    pub(crate) fn is_healthy(&self) -> bool {
        !self.tx.is_closed() && self.join.as_ref().is_some_and(|join| !join.is_finished())
    }

    pub(crate) async fn shutdown(&mut self, timeout_budget: Duration) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        let mut join = crate::engine::ShutdownTaskGuard {
            task: self.join.take(),
        };
        let Some(task) = join.task.as_mut() else {
            return;
        };
        let _ = crate::shutdown_join_task(
            task,
            timeout_budget,
            STORAGE_WORKER_ABORT_GRACE,
            "storage_jobs",
            "shutdown",
            "storage worker supervisor",
            || {},
        )
        .await;
    }
}

impl Drop for StorageJobDispatcher {
    fn drop(&mut self) {
        // The supervisor may be dropped while the engine actor is being
        // aborted. Mark every admitted request before aborting that task so a
        // currently running `spawn_blocking` filesystem execution can still
        // observe shutdown at its next step boundary and persist a queued
        // recovery state instead of continuing as an unowned write.
        let controls = lock_storage_job_controls(&self.controls);
        for control in controls.values() {
            control.apply(StorageJobAction::Shutdown);
        }
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

async fn run_supervisor(
    mut rx: mpsc::Receiver<StorageJobRequest>,
    mut shutdown_rx: oneshot::Receiver<()>,
    slots: Arc<Semaphore>,
    controls: Arc<Mutex<HashMap<String, Arc<StorageJobControl>>>>,
    executor: StorageJobExecutor,
) {
    let mut active = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                // Close the receiver before draining active work. A sender
                // can otherwise enqueue a request after the final
                // `try_recv` below and leave it permanently without a
                // supervisor or completion channel.
                rx.close();
                let active_controls = lock_storage_job_controls(&controls)
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for control in active_controls {
                    control.apply(StorageJobAction::Shutdown);
                }
                // Active requests are not all blocking workers: requests over
                // the worker count are supervisor tasks waiting for a slot.
                // A wedged blocking worker must not keep those waiters
                // pending until this supervisor itself is aborted. Closing
                // the semaphore wakes every waiter so it can persist its
                // queued recovery state and release its completion channel.
                slots.close();
                break;
            },
            Some(result) = active.join_next(), if !active.is_empty() => {
                match result {
                    Ok(job_id) => {
                        lock_storage_job_controls(&controls).remove(&job_id);
                    }
                    Err(error) => {
                        warn!(
                            component = "storage_jobs",
                            operation = "join",
                            result = "error",
                            error = %crate::task_join_error_summary("storage supervisor task", &error),
                            "storage worker join failed"
                        );
                    }
                }
            }
            Some(request) = rx.recv() => {
                let slots = Arc::clone(&slots);
                let request_controls = Arc::clone(&controls);
                let executor = Arc::clone(&executor);
                active.spawn(async move {
                    run_storage_request(request, slots, request_controls, executor).await
                });
            }
            else => break,
        }
    }

    while let Some(result) = active.join_next().await {
        match result {
            Ok(job_id) => {
                lock_storage_job_controls(&controls).remove(&job_id);
            }
            Err(error) => {
                warn!(
                    component = "storage_jobs",
                    operation = "join",
                    result = "error",
                    error = %crate::task_join_error_summary("storage supervisor task", &error),
                    "storage worker join failed during drain"
                );
            }
        }
    }

    // Requests still in the bounded queue never started. Keep them queued so
    // a process restart can reattach and execute them; an explicit user
    // cancellation still wins over the supervisor shutdown that woke the
    // drain, so it must not be silently converted into restartable work.
    while let Ok(request) = rx.try_recv() {
        let completion = if request.control.cancelled.load(Ordering::Acquire) {
            cancelled_before_start_completion_async(
                &request.db,
                &request.job_id,
                &request.operation,
                &request.plan,
                &request.roots,
                &request.completed_steps,
            )
            .await
        } else {
            interrupted_before_start_completion(
                &request.db,
                &request.job_id,
                &request.operation,
                &request.plan,
                &request.roots,
                &request.completed_steps,
                true,
            )
        };
        let _ = request.completion.send(completion);
        lock_storage_job_controls(&controls).remove(&request.job_id);
    }
}

async fn run_storage_request(
    request: StorageJobRequest,
    slots: Arc<Semaphore>,
    controls: Arc<Mutex<HashMap<String, Arc<StorageJobControl>>>>,
    executor: StorageJobExecutor,
) -> String {
    let StorageJobRequest {
        job_id,
        operation,
        plan,
        completed_steps,
        roots,
        db,
        control,
        completion,
    } = request;
    let _registration = StorageJobRegistration {
        job_id: job_id.clone(),
        controls,
    };
    let mut completed_steps = completed_steps;
    loop {
        control.wait_until_runnable().await;
        control.wait_for_fault_delay().await;
        let recovery_db = Arc::clone(&db);
        let recovery_operation = operation.clone();
        let recovery_plan = plan.clone();
        let recovery_completed_steps = completed_steps.clone();
        let slot = match Arc::clone(&slots).acquire_owned().await {
            Ok(slot) => slot,
            Err(error) => {
                if control.cancelled.load(Ordering::Acquire) {
                    let completion_result = cancelled_before_start_completion_async(
                        &db,
                        &job_id,
                        &operation,
                        &plan,
                        &roots,
                        &completed_steps,
                    )
                    .await;
                    let _ = completion.send(completion_result);
                    return job_id;
                }
                if control.is_shutdown() {
                    let completion_result = interrupted_before_start_completion(
                        &db,
                        &job_id,
                        &operation,
                        &plan,
                        &roots,
                        &completed_steps,
                        true,
                    );
                    let _ = completion.send(completion_result);
                    return job_id;
                }
                let reason = format!("storage worker semaphore closed: {error}");
                let requires_manual_recovery = storage_plan_has_destructive_progress(
                    &recovery_plan,
                    &recovery_completed_steps,
                );
                let durable_reason = if requires_manual_recovery {
                    manual_recovery_reason(&reason)
                } else {
                    reason.clone()
                };
                let _ = persist_terminal(
                    &recovery_db,
                    &job_id,
                    &recovery_operation,
                    &recovery_plan,
                    &recovery_completed_steps,
                    "failed",
                    Some(durable_reason.clone()),
                );
                let completion_result = if requires_manual_recovery {
                    StorageJobCompletion::failed_with_manual_recovery(
                        durable_reason,
                        recovery_completed_steps,
                    )
                } else {
                    StorageJobCompletion::failed(reason, recovery_completed_steps)
                };
                let _ = completion.send(completion_result);
                return job_id;
            }
        };
        // A pause can arrive after the wait and before the slot is acquired.
        // Release the slot and wait again so paused jobs do not starve active
        // work behind the fixed worker budget.
        if control.is_paused()
            && !control.cancelled.load(Ordering::Acquire)
            && !control.is_shutdown()
        {
            drop(slot);
            continue;
        }
        if control.is_shutdown() || control.cancelled.load(Ordering::Acquire) {
            drop(slot);
            let completion_result = if control.cancelled.load(Ordering::Acquire) {
                cancelled_before_start_completion_async(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &roots,
                    &completed_steps,
                )
                .await
            } else {
                interrupted_before_start_completion(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &roots,
                    &completed_steps,
                    true,
                )
            };
            let _ = completion.send(completion_result);
            return job_id;
        }
        let execution = StorageJobExecution {
            job_id: job_id.clone(),
            operation: operation.clone(),
            plan: plan.clone(),
            completed_steps: completed_steps.clone(),
            roots: roots.clone(),
            db: Arc::clone(&db),
            control: Arc::clone(&control),
        };
        let job_executor = Arc::clone(&executor);
        let result = tokio::task::spawn_blocking(move || job_executor(execution)).await;
        drop(slot);
        let completion_result = match result {
            Ok(completion_result) => completion_result,
            Err(error) => {
                let reason = crate::task_join_error_summary("storage worker", &error);
                let durable_reason = manual_recovery_reason(&reason);
                let _ = persist_terminal(
                    &recovery_db,
                    &job_id,
                    &recovery_operation,
                    &recovery_plan,
                    &recovery_completed_steps,
                    "failed",
                    Some(durable_reason.clone()),
                );
                warn!(
                    component = "storage_jobs",
                    operation = "worker",
                    job_id = %job_id,
                    result = "panic",
                    error = %reason,
                    "storage worker task failed; durable job marked failed"
                );
                StorageJobCompletion::failed_with_manual_recovery(
                    durable_reason,
                    recovery_completed_steps,
                )
            }
        };
        if completion_result.state == "paused" {
            if control.cancelled.load(Ordering::Acquire) {
                // Cancellation can arrive after the blocking worker has
                // computed a paused result but before this supervisor task
                // observes it. Re-run the terminal cancellation path so a
                // staged checkpoint is cleaned up and the job cannot remain
                // paused forever waiting for a resume that will never come.
                let completion_result = interrupted_before_start_completion(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &roots,
                    &completion_result.completed_steps,
                    false,
                );
                let _ = completion.send(completion_result);
                return job_id;
            }
            if control.is_shutdown() {
                // The worker may have observed a user pause just before the
                // supervisor applied shutdown. Convert that already-persisted
                // paused checkpoint back to queued work so restart recovery
                // cannot strand it in a dormant paused state.
                let completion_result = interrupted_before_start_completion(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &roots,
                    &completion_result.completed_steps,
                    true,
                );
                let _ = completion.send(completion_result);
                return job_id;
            }
            completed_steps = completion_result.completed_steps;
            // The blocking worker and its semaphore slot are released before
            // waiting. Resume wakes this task and creates a fresh execution
            // attempt from the durable checkpoint.
            control.wait_until_runnable().await;
            continue;
        }
        let _ = completion.send(completion_result);
        return job_id;
    }
}

fn interrupted_before_start_completion(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    roots: &[std::path::PathBuf],
    completed_steps: &[usize],
    shutdown_requested: bool,
) -> StorageJobCompletion {
    if shutdown_requested {
        let reason = "storage worker shutdown; job will be recovered after restart";
        return match persist_requeued(db, job_id, operation, plan, completed_steps, reason) {
            Ok(()) => StorageJobCompletion::requeued(reason, completed_steps.to_vec()),
            Err(error) => {
                let reason = format!("{reason}; failed to persist requeued state: {error}");
                warn!(
                    component = "storage_jobs",
                    operation = "persist_requeued",
                    job_id = %job_id,
                    result = "error",
                    error = %error,
                    "storage job was interrupted before start and could not persist its queued state"
                );
                StorageJobCompletion::requeued(reason, completed_steps.to_vec())
            }
        };
    }

    // A cancellation can race the final filesystem syscall before this
    // worker reaches its normal terminal-state calculation. A complete plan
    // is already live on disk and must still enter the actor-side commit path;
    // otherwise a move can be marked cancelled while its old source is gone,
    // or a payload delete can be marked complete before its metadata is gone.
    if !plan.steps.is_empty() && storage_plan_filesystem_complete(plan, completed_steps) {
        let state = if matches!(operation, "move" | STORAGE_JOB_OPERATION_PAYLOAD_DELETE) {
            STORAGE_JOB_STATE_COMMIT_PENDING
        } else {
            "completed"
        };
        let completed_offset = Some(completed_byte_offset(plan, completed_steps));
        return match persist_terminal(db, job_id, operation, plan, completed_steps, state, None) {
            Ok(()) => {
                let mut completion =
                    StorageJobCompletion::terminal(state, None, completed_steps.to_vec());
                completion.completed_byte_offset = completed_offset;
                completion
            }
            Err(error) => {
                let mut completion = StorageJobCompletion::terminal(
                    STORAGE_JOB_STATE_COMMIT_PENDING,
                    Some(format!("terminal persistence failed: {error}")),
                    completed_steps.to_vec(),
                );
                completion.completed_byte_offset = completed_offset;
                completion
            }
        };
    }

    let cancellation = cancelled_error();
    let cleanup_failure = cancellation_cleanup_failure(plan, roots, completed_steps);
    let requires_manual_recovery = cleanup_failure.is_some()
        || storage_plan_failure_requires_manual_recovery(
            plan,
            completed_steps,
            Some(&cancellation),
        );
    let reason = cleanup_failure.map_or_else(
        || cancellation.to_string(),
        |failure| format!("{cancellation}; cancelled-job cleanup failed: {failure}"),
    );
    let durable_reason = if requires_manual_recovery {
        manual_recovery_reason(&reason)
    } else {
        reason.clone()
    };
    let terminal_state = if requires_manual_recovery {
        "failed"
    } else {
        "cancelled"
    };
    match persist_terminal(
        db,
        job_id,
        operation,
        plan,
        completed_steps,
        terminal_state,
        Some(durable_reason.clone()),
    ) {
        Ok(()) if requires_manual_recovery => StorageJobCompletion::failed_with_manual_recovery(
            durable_reason,
            completed_steps.to_vec(),
        ),
        Ok(()) => {
            StorageJobCompletion::terminal("cancelled", Some(reason), completed_steps.to_vec())
        }
        Err(error) => {
            let failure = format!("{durable_reason}; failed to persist cancelled state: {error}");
            warn!(
                component = "storage_jobs",
                operation = "persist_cancelled",
                job_id = %job_id,
                result = "error",
                error = %error,
                "storage job was cancelled before start and could not persist its terminal state"
            );
            if requires_manual_recovery {
                StorageJobCompletion::failed_with_manual_recovery(failure, completed_steps.to_vec())
            } else {
                StorageJobCompletion::failed(failure, completed_steps.to_vec())
            }
        }
    }
}

/// Reconcile the filesystem before cleaning up a cancellation that arrived
/// before the normal execution path started. A checkpoint can lag behind a
/// completed filesystem syscall, so trusting only `completed_steps` here can
/// strand a staging file after a crash.
fn cancelled_before_start_completion(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    roots: &[std::path::PathBuf],
    completed_steps: &[usize],
) -> StorageJobCompletion {
    let completed_steps = if plan.steps.is_empty() {
        completed_steps.to_vec()
    } else {
        match rt_storage::reconcile_storage_plan_under_roots(plan, roots, completed_steps) {
            Ok(completed_steps) => completed_steps,
            Err(error) => {
                return cancelled_reconciliation_failure(
                    db,
                    job_id,
                    operation,
                    plan,
                    completed_steps,
                    &error,
                )
            }
        }
    };
    interrupted_before_start_completion(db, job_id, operation, plan, roots, &completed_steps, false)
}

async fn cancelled_before_start_completion_async(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    roots: &[std::path::PathBuf],
    completed_steps: &[usize],
) -> StorageJobCompletion {
    let db = Arc::clone(db);
    let job_id = job_id.to_owned();
    let operation = operation.to_owned();
    let plan = plan.clone();
    let roots = roots.to_vec();
    let completed_steps = completed_steps.to_vec();
    let fallback_db = Arc::clone(&db);
    let fallback_job_id = job_id.clone();
    let fallback_operation = operation.clone();
    let fallback_plan = plan.clone();
    let fallback_completed_steps = completed_steps.clone();
    match tokio::task::spawn_blocking(move || {
        cancelled_before_start_completion(&db, &job_id, &operation, &plan, &roots, &completed_steps)
    })
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            let reason =
                crate::task_join_error_summary("storage cancellation cleanup worker", &error);
            let durable_reason = manual_recovery_reason(&reason);
            let _ = persist_terminal(
                &fallback_db,
                &fallback_job_id,
                &fallback_operation,
                &fallback_plan,
                &fallback_completed_steps,
                "failed",
                Some(durable_reason.clone()),
            );
            StorageJobCompletion::failed_with_manual_recovery(
                durable_reason,
                fallback_completed_steps,
            )
        }
    }
}

fn cancelled_reconciliation_failure(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
    error: &StorageError,
) -> StorageJobCompletion {
    let reason = format!("storage plan cancellation reconciliation failed: {error}");
    let durable_reason = manual_recovery_reason(&reason);
    let _ = persist_terminal(
        db,
        job_id,
        operation,
        plan,
        completed_steps,
        "failed",
        Some(durable_reason.clone()),
    );
    StorageJobCompletion::failed_with_manual_recovery(durable_reason, completed_steps.to_vec())
}

fn execute_storage_job(request: StorageJobExecution) -> StorageJobCompletion {
    let StorageJobExecution {
        job_id,
        operation,
        plan,
        completed_steps,
        roots,
        db,
        control,
    } = request;
    if control.cancelled.load(Ordering::Acquire) {
        return cancelled_before_start_completion(
            &db,
            &job_id,
            &operation,
            &plan,
            &roots,
            &completed_steps,
        );
    }
    if control.is_shutdown() {
        return interrupted_before_start_completion(
            &db,
            &job_id,
            &operation,
            &plan,
            &roots,
            &completed_steps,
            true,
        );
    }
    // Filesystem reconciliation is deliberately inside the blocking worker.
    // It may stat many plan paths and must not run in the engine actor before
    // a request is admitted to the detached storage queue.
    let mut completed =
        match rt_storage::reconcile_storage_plan_under_roots(&plan, &roots, &completed_steps) {
            Ok(completed) => completed,
            Err(error) => {
                if control.cancelled.load(Ordering::Acquire) {
                    return cancelled_reconciliation_failure(
                        &db,
                        &job_id,
                        &operation,
                        &plan,
                        &completed_steps,
                        &error,
                    );
                }
                if control.is_shutdown() {
                    return interrupted_before_start_completion(
                        &db,
                        &job_id,
                        &operation,
                        &plan,
                        &roots,
                        &completed_steps,
                        control.is_shutdown() && !control.cancelled.load(Ordering::Acquire),
                    );
                }
                let requires_manual_recovery = storage_plan_failure_requires_manual_recovery(
                    &plan,
                    &completed_steps,
                    Some(&error),
                );
                let reason = format!("storage plan checkpoint reconciliation failed: {error}");
                let durable_reason = if requires_manual_recovery {
                    manual_recovery_reason(&reason)
                } else {
                    reason.clone()
                };
                let _ = persist_terminal(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &completed_steps,
                    "failed",
                    Some(durable_reason.clone()),
                );
                return if requires_manual_recovery {
                    StorageJobCompletion::failed_with_manual_recovery(
                        durable_reason,
                        completed_steps,
                    )
                } else {
                    StorageJobCompletion::failed(reason, completed_steps)
                };
            }
        };
    if control.cancelled.load(Ordering::Acquire) || control.is_shutdown() {
        if control.cancelled.load(Ordering::Acquire) {
            return interrupted_before_start_completion(
                &db, &job_id, &operation, &plan, &roots, &completed, false,
            );
        }
        return interrupted_before_start_completion(
            &db, &job_id, &operation, &plan, &roots, &completed, true,
        );
    }
    let already_completed = completed.clone();
    match persist_running(&db, &job_id) {
        Ok(PersistRunningResult::Started) => {}
        Ok(PersistRunningResult::CancellationRequested) => {
            // Cancellation is durably recorded before the engine applies the
            // in-memory worker control. If this worker wins the race and
            // observes that durable fence first, never overwrite it with
            // `running`; reconcile and finish the cancellation path instead.
            return cancelled_before_start_completion(
                &db, &job_id, &operation, &plan, &roots, &completed,
            );
        }
        Err(error) => {
            warn!(
                component = "storage_jobs",
                operation = "persist_running",
                job_id = %job_id,
                result = "error",
                error = %error,
                "storage job did not start because its durable running state could not be written"
            );
            let reason = format!("failed to persist storage job running state: {error}");
            let requires_manual_recovery =
                storage_plan_failure_requires_manual_recovery(&plan, &completed, None);
            let durable_reason = if requires_manual_recovery {
                manual_recovery_reason(&reason)
            } else {
                reason.clone()
            };
            // `persist_running` may fail after its transaction has rolled back.
            // Make a best-effort terminal write so a transient trigger/SQLite
            // failure does not leave the durable job looking queued even though
            // this worker has already abandoned it.
            let _ = persist_terminal(
                &db,
                &job_id,
                &operation,
                &plan,
                &completed,
                "failed",
                Some(durable_reason.clone()),
            );
            return if requires_manual_recovery {
                StorageJobCompletion::failed_with_manual_recovery(durable_reason, completed.clone())
            } else {
                StorageJobCompletion::failed(reason, completed.clone())
            };
        }
    }

    let checkpoint = |index: usize, _step: &StoragePlanStep| {
        if !completed.contains(&index) {
            completed.push(index);
            completed.sort_unstable();
        }
        persist_checkpoint(&db, &job_id, &operation, &plan, &completed).map_err(|error| {
            StorageError::StagedMoveFailed {
                step: "checkpoint",
                reason: error,
            }
        })?;
        control.check_step_boundary()
    };
    let check_control = || control.check_step_boundary();
    let result = if control.cancelled.load(Ordering::Acquire) {
        Err(cancelled_error())
    } else if control.is_shutdown() {
        Err(shutdown_error())
    } else {
        rt_storage::execute_storage_plan_under_roots_with_checkpoints_and_control(
            &plan,
            &roots,
            &already_completed,
            checkpoint,
            check_control,
        )
    };
    // Once the plan has returned successfully, all filesystem steps have
    // completed. A cancellation arriving in that tiny hand-off window must
    // not relabel committed work as cancelled; doing so would leave a job
    // claiming cancellation even though its side effects are already live.
    let cancelled = storage_job_was_cancelled(
        control.cancelled.load(Ordering::Acquire),
        &result,
        &plan,
        &completed,
    );
    let shutdown_requested = control.is_shutdown() && !control.cancelled.load(Ordering::Acquire);
    // `checkpoint` records the step in `completed` before it writes the
    // durable row and observes control state. Therefore a cancellation or
    // checkpoint error can still arrive after the final filesystem syscall.
    // A move in that state is already committed on disk and must enter the
    // actor-side commit path; resuming its task against the old root would
    // split the filesystem and torrent projections.
    let filesystem_complete = storage_plan_filesystem_complete(&plan, &completed);
    let paused = !shutdown_requested
        && !cancelled
        && result.as_ref().is_err_and(|error| {
            matches!(error, StorageError::StagedMoveFailed { step: "pause", .. })
        });
    let requires_manual_recovery = !filesystem_complete
        && !shutdown_requested
        && !paused
        && storage_plan_failure_requires_manual_recovery(&plan, &completed, result.as_ref().err());
    let terminal_state = storage_job_terminal_state(
        &operation,
        filesystem_complete,
        shutdown_requested,
        cancelled,
        paused,
        result.is_ok(),
    );
    let error = result.err().map(|error| error.to_string());
    let persisted_error = paused.then_some("storage plan paused at a step boundary".to_owned());
    let mut completion_state = terminal_state.to_owned();
    let mut completion_error = if filesystem_complete || paused {
        None
    } else {
        error.clone().map(|error| {
            if requires_manual_recovery {
                manual_recovery_reason(error)
            } else {
                error
            }
        })
    };
    let persistence = if shutdown_requested {
        persist_requeued(
            &db,
            &job_id,
            &operation,
            &plan,
            &completed,
            "storage worker shutdown; job will be recovered after restart",
        )
    } else {
        persist_terminal(
            &db,
            &job_id,
            &operation,
            &plan,
            &completed,
            terminal_state,
            if filesystem_complete {
                None
            } else {
                persisted_error.or_else(|| {
                    error.map(|error| {
                        if requires_manual_recovery {
                            manual_recovery_reason(error)
                        } else {
                            error
                        }
                    })
                })
            },
        )
    };
    if let Err(persist_error) = persistence {
        warn!(
            component = "storage_jobs",
            operation = "persist_terminal",
            job_id = %job_id,
            result = "error",
            error = %persist_error,
            "failed to persist storage job terminal state"
        );
        // A successful move has committed the filesystem even if this
        // worker could not persist its own terminal row. Preserve
        // `commit_pending` so the actor keeps the destination projection
        // live and can retry the durable DB commit; treating this as a
        // normal failure would resume peers against the old, now-missing
        // root.
        completion_state = if shutdown_requested {
            "queued".to_owned()
        } else if filesystem_complete {
            STORAGE_JOB_STATE_COMMIT_PENDING.to_owned()
        } else {
            "failed".to_owned()
        };
        completion_error = Some(match completion_error {
            Some(error) => format!("{error}; terminal persistence failed: {persist_error}"),
            None => format!("terminal persistence failed: {persist_error}"),
        });
        if requires_manual_recovery {
            completion_error = completion_error.map(manual_recovery_reason);
        }
    }
    debug!(
        component = "storage_jobs",
        operation = "execute",
        job_id = %job_id,
        state = terminal_state,
        done = completed.len(),
        "storage plan worker finished"
    );
    let completed_offset = completed_byte_offset(&plan, &completed);
    StorageJobCompletion {
        succeeded: matches!(completion_state.as_str(), STORAGE_JOB_STATE_COMMIT_PENDING)
            || (completion_state == "completed" && completion_error.is_none()),
        state: completion_state,
        error: completion_error,
        completed_steps: completed,
        completed_byte_offset: Some(completed_offset),
        requires_manual_recovery,
    }
}

fn persist_requeued(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
    reason: &str,
) -> Result<(), String> {
    let mut conn = lock_storage_job_db(db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut job = rt_db::get_job(&tx, job_id).map_err(|error| error.to_string())?;
    if job.finished_at.is_some()
        || matches!(
            job.state.as_str(),
            "cancelled" | "completed" | "failed" | STORAGE_JOB_STATE_COMMIT_PENDING
        )
    {
        return Err(format!("job {job_id} is already terminal"));
    }
    job.state = "queued".to_owned();
    job.done = db_i64_usize(completed_steps.len());
    job.checkpoint = job.done;
    job.file_index = Some(job.done);
    job.byte_offset = Some(completed_byte_offset(plan, completed_steps));
    job.error = Some(reason.to_owned());
    job.updated_at = unix_now_i64();
    job.finished_at = None;
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: "storage_plan_requeued".to_owned(),
        message: Some(reason.to_owned()),
        payload: storage_plan_progress_payload(operation, completed_steps).to_string(),
    };
    rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn cancelled_error() -> StorageError {
    StorageError::StagedMoveFailed {
        step: "cancel",
        reason: "storage plan cancelled".to_owned(),
    }
}

fn paused_error() -> StorageError {
    StorageError::StagedMoveFailed {
        step: "pause",
        reason: "storage plan paused at a step boundary".to_owned(),
    }
}

fn shutdown_error() -> StorageError {
    StorageError::StagedMoveFailed {
        step: "shutdown",
        reason: "storage plan interrupted by worker shutdown".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistRunningResult {
    Started,
    CancellationRequested,
}

fn persist_running(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
) -> Result<PersistRunningResult, String> {
    let mut conn = lock_storage_job_db(db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut job = rt_db::get_job(&tx, job_id).map_err(|error| error.to_string())?;
    if job.finished_at.is_some()
        || matches!(
            job.state.as_str(),
            "cancelled" | "completed" | "failed" | STORAGE_JOB_STATE_COMMIT_PENDING
        )
    {
        return Err(format!("job {job_id} is already terminal"));
    }
    if job.state == "cancelling" {
        // The engine persists cancellation intent before setting the worker's
        // atomic flag. Preserve that durable fence if this transaction wins
        // the race with the control call.
        return Ok(PersistRunningResult::CancellationRequested);
    }
    job.state = "running".to_owned();
    if job.started_at.is_none() {
        job.started_at = Some(unix_now_i64());
    }
    // A queued job can carry the previous shutdown/recovery explanation.
    // Once execution has actually resumed, that message is no longer the
    // current job status and must not remain visible beside `running`.
    job.error = None;
    job.updated_at = unix_now_i64();
    rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: "storage_plan_running".to_owned(),
        message: Some("storage plan started".to_owned()),
        payload: serde_json::json!({ "state": job.state }).to_string(),
    };
    rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
    tx.commit()
        .map(|()| PersistRunningResult::Started)
        .map_err(|error| error.to_string())
}

fn persist_checkpoint(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> Result<(), String> {
    let mut conn = lock_storage_job_db(db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut job = rt_db::get_job(&tx, job_id).map_err(|error| error.to_string())?;
    if job.finished_at.is_some()
        || matches!(
            job.state.as_str(),
            "cancelled" | "completed" | "failed" | STORAGE_JOB_STATE_COMMIT_PENDING
        )
    {
        return Err(format!("job {job_id} is already terminal"));
    }
    job.done = db_i64_usize(completed_steps.len());
    job.checkpoint = job.done;
    job.file_index = Some(job.done);
    job.byte_offset = Some(completed_byte_offset(plan, completed_steps));
    job.updated_at = unix_now_i64();
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: "storage_plan_checkpoint".to_owned(),
        message: Some("storage plan checkpoint persisted".to_owned()),
        payload: storage_plan_progress_payload(operation, completed_steps).to_string(),
    };
    rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn persist_terminal(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
    state: &str,
    error: Option<String>,
) -> Result<(), String> {
    let mut conn = lock_storage_job_db(db)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut job = rt_db::get_job(&tx, job_id).map_err(|error| error.to_string())?;
    if job.finished_at.is_some()
        || matches!(
            job.state.as_str(),
            "cancelled" | "completed" | "failed" | STORAGE_JOB_STATE_COMMIT_PENDING
        )
    {
        if job.state == state {
            return Ok(());
        }
        return Err(format!("job {job_id} is already terminal"));
    }
    job.state = state.to_owned();
    job.done = db_i64_usize(completed_steps.len());
    job.checkpoint = job.done;
    job.file_index = Some(job.done);
    job.byte_offset = Some(completed_byte_offset(plan, completed_steps));
    job.error = error.clone();
    job.updated_at = unix_now_i64();
    job.finished_at = (!matches!(
        state,
        STORAGE_JOB_STATE_COMMIT_PENDING | "paused" | "queued"
    ))
    .then_some(job.updated_at);
    let mut payload = storage_plan_progress_payload(operation, completed_steps);
    payload["state"] = serde_json::Value::String(state.to_owned());
    payload["error"] = error
        .clone()
        .map_or(serde_json::Value::Null, serde_json::Value::String);
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: format!("storage_plan_{state}"),
        message: Some(format!("storage plan {state}")),
        payload: payload.to_string(),
    };
    rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

/// Checkpoint events are emitted once per completed filesystem step. They
/// must not repeat the full plan or the entire completed-step prefix, or a
/// large torrent turns durable progress into quadratic disk and allocation
/// growth. The queue event retains the authoritative full plan; recovery can
/// reconcile this latest step against the filesystem.
fn storage_plan_progress_payload(operation: &str, completed_steps: &[usize]) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "operation": operation,
        "completed_count": completed_steps.len(),
    });
    if let Some(index) = completed_steps.last().copied() {
        payload["completed_step"] = serde_json::json!(index);
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Barrier;

    use rt_db::migrate;
    use rt_storage::{plan_import, plan_move, ImportPlanRequest, MovePlanRequest};

    #[test]
    fn poisoned_controls_mutex_is_recovered_for_worker_cleanup() {
        let controls = Arc::new(Mutex::new(HashMap::new()));
        let controls_to_poison = Arc::clone(&controls);
        let panic = std::panic::catch_unwind(move || {
            let _guard = controls_to_poison.lock().unwrap();
            panic!("inject control-map panic while locked");
        });
        assert!(panic.is_err());

        let control = Arc::new(StorageJobControl::new(false));
        lock_storage_job_controls(&controls).insert("recoverable".to_owned(), control);
        assert!(!controls.is_poisoned());
        assert!(lock_storage_job_controls(&controls).contains_key("recoverable"));
    }

    #[test]
    fn poisoned_database_mutex_is_reported_instead_of_panicking() {
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let db_to_poison = Arc::clone(&db);
        let panic = std::panic::catch_unwind(move || {
            let _guard = db_to_poison.lock().unwrap();
            panic!("inject database panic while locked");
        });
        assert!(panic.is_err());

        let error = persist_running(&db, "poisoned-lock-test").unwrap_err();
        assert!(error.contains("database mutex poisoned during storage job persistence"));
    }

    #[tokio::test]
    async fn queue_is_bounded_and_control_is_shared() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let db = Arc::new(Mutex::new(connection));
        let mut dispatcher = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 1);
        let control = Arc::new(StorageJobControl::new(false));
        control.apply(StorageJobAction::Pause);
        assert!(control.paused.load(Ordering::Acquire));
        control.apply(StorageJobAction::Resume);
        assert!(!control.paused.load(Ordering::Acquire));
        control.apply(StorageJobAction::Cancel);
        assert!(control.cancelled.load(Ordering::Acquire));
        let shutdown_control = Arc::new(StorageJobControl::new(false));
        shutdown_control.apply(StorageJobAction::Shutdown);
        assert!(shutdown_control.is_shutdown());
        assert!(!shutdown_control.cancelled.load(Ordering::Acquire));
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[test]
    fn dropping_dispatcher_interrupts_admitted_jobs_before_abort() {
        let dispatcher = StorageJobDispatcher::for_tests();
        let control = Arc::new(StorageJobControl::new(false));
        dispatcher
            .controls
            .lock()
            .unwrap()
            .insert("drop-active".to_owned(), Arc::clone(&control));

        drop(dispatcher);

        assert!(control.is_shutdown());
    }

    #[tokio::test]
    async fn cancellation_before_worker_start_does_not_run_storage_plan() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "cancel-before-start".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "paused".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let executed = Arc::new(AtomicBool::new(false));
        let executor: StorageJobExecutor = {
            let executed = Arc::clone(&executed);
            Arc::new(move |execution| {
                executed.store(true, Ordering::Release);
                execute_storage_job(execution)
            })
        };
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit_paused(
                Arc::clone(&db),
                "cancel-before-start".to_owned(),
                "move".to_owned(),
                plan,
                Vec::new(),
                vec![],
                completion,
            )
            .unwrap();
        dispatcher
            .control("cancel-before-start", StorageJobAction::Cancel)
            .unwrap();
        // A shutdown racing with an explicit cancellation must not turn the
        // user's terminal request back into restartable queued work.
        dispatcher.shutdown(Duration::from_secs(1)).await;

        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.state, "cancelled");
        assert!(!completion.succeeded);
        assert!(!executed.load(Ordering::Acquire));
        let job = rt_db::get_job(&db.lock().unwrap(), "cancel-before-start").unwrap();
        assert_eq!(job.state, "cancelled");
        assert!(job.finished_at.is_some());
    }

    #[tokio::test]
    async fn recovered_cancelling_job_cleans_staging_without_running_plan() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination,
            bytes: 4,
            available_bytes: None,
            hardlink_or_copy: false,
            dry_run: false,
        });
        let staged = plan.steps[0].destination.clone().unwrap();
        std::fs::write(&staged, b"data").unwrap();

        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "recovered-cancelling".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "cancelling".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: plan.steps.len() as i64,
                // The filesystem syscall can complete before its checkpoint
                // transaction commits, so recovery must not trust this row
                // to describe the staging file below.
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: Some("storage plan cancellation requested".to_owned()),
                created_at: now,
                started_at: Some(now),
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let executed = Arc::new(AtomicBool::new(false));
        let executor: StorageJobExecutor = {
            let executed = Arc::clone(&executed);
            Arc::new(move |execution| {
                executed.store(true, Ordering::Release);
                execute_storage_job(execution)
            })
        };
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit_cancelled(
                Arc::clone(&db),
                "recovered-cancelling".to_owned(),
                "import".to_owned(),
                plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();

        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.state, "cancelled");
        assert!(!completion.succeeded);
        assert!(!executed.load(Ordering::Acquire));
        assert!(source.exists());
        assert!(!staged.exists());
        let job = rt_db::get_job(&db.lock().unwrap(), "recovered-cancelling").unwrap();
        assert_eq!(job.state, "cancelled");
        assert!(job.finished_at.is_some());
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[test]
    fn filesystem_completion_wins_over_a_late_control_error() {
        assert_eq!(
            storage_job_terminal_state("move", true, false, true, false, false),
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
        assert_eq!(
            storage_job_terminal_state("import", true, false, true, false, false),
            "completed"
        );
        assert_eq!(
            storage_job_terminal_state(
                STORAGE_JOB_OPERATION_PAYLOAD_DELETE,
                true,
                false,
                false,
                false,
                true,
            ),
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
        assert_eq!(
            storage_job_terminal_state("move", false, false, true, false, false),
            "cancelled"
        );
        assert_eq!(
            storage_job_terminal_state("move", false, false, false, false, true),
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
    }

    #[test]
    fn durable_cancellation_fences_storage_worker_start() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "durable-cancel-fence".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "cancelling".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: Some("storage plan cancellation requested".to_owned()),
                created_at: now,
                started_at: Some(now),
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let root = tempfile::tempdir().unwrap();
        let completion = execute_storage_job(StorageJobExecution {
            job_id: "durable-cancel-fence".to_owned(),
            operation: "import".to_owned(),
            plan: StoragePlan {
                dry_run: false,
                can_apply: true,
                issues: Vec::new(),
                steps: Vec::new(),
                rollback_steps: Vec::new(),
            },
            completed_steps: Vec::new(),
            roots: vec![root.path().to_path_buf()],
            db: Arc::clone(&db),
            control: Arc::new(StorageJobControl::new(false)),
        });

        assert_eq!(completion.state, "cancelled");
        assert!(!completion.succeeded);
        let job = rt_db::get_job(&db.lock().unwrap(), "durable-cancel-fence").unwrap();
        assert_eq!(job.state, "cancelled");
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn cancellation_only_wins_before_destructive_progress() {
        let copy_plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: rt_storage::PlannedStorageAction::CopyVerifyRename,
                source: None,
                destination: None,
                bytes: 0,
            }],
            rollback_steps: Vec::new(),
        };
        let destructive_plan = StoragePlan {
            steps: vec![StoragePlanStep {
                action: rt_storage::PlannedStorageAction::Rename,
                source: None,
                destination: None,
                bytes: 0,
            }],
            ..copy_plan.clone()
        };
        let cancellation = StorageError::StagedMoveFailed {
            step: "cancel",
            reason: "injected cancellation".to_owned(),
        };
        let cancelled_result = || {
            Err::<rt_storage::StoragePlanExecution, _>(StorageError::StagedMoveFailed {
                step: "cancel",
                reason: "injected cancellation".to_owned(),
            })
        };

        assert!(storage_job_was_cancelled(
            true,
            &cancelled_result(),
            &copy_plan,
            &[0],
        ));
        assert!(!storage_job_was_cancelled(
            true,
            &cancelled_result(),
            &destructive_plan,
            &[0],
        ));
        assert!(!storage_job_was_cancelled(
            true,
            &Err(StorageError::Io {
                path: "source".to_owned(),
                source: std::io::Error::other("injected I/O failure"),
            }),
            &copy_plan,
            &[0],
        ));
        assert!(!storage_plan_failure_requires_manual_recovery(
            &copy_plan,
            &[0],
            Some(&cancellation),
        ));
        assert!(storage_plan_failure_requires_manual_recovery(
            &destructive_plan,
            &[0],
            Some(&cancellation),
        ));
    }

    #[test]
    fn cancellation_wins_over_a_late_pause_boundary() {
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![StoragePlanStep {
                action: rt_storage::PlannedStorageAction::CopyVerifyRename,
                source: None,
                destination: None,
                bytes: 0,
            }],
            rollback_steps: Vec::new(),
        };
        let paused_result = Err(StorageError::StagedMoveFailed {
            step: "pause",
            reason: "pause arrived at a step boundary".to_owned(),
        });

        assert!(storage_job_was_cancelled(true, &paused_result, &plan, &[]));
        assert!(!storage_job_was_cancelled(
            false,
            &paused_result,
            &plan,
            &[]
        ));
    }

    #[test]
    fn cancelled_checkpointed_staging_is_cleaned_before_terminal_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_import(&ImportPlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            hardlink_or_copy: false,
            dry_run: false,
        });
        std::fs::write(plan.steps[0].destination.clone().unwrap(), b"data").unwrap();
        let roots = vec![root.path().to_path_buf()];

        assert!(cancellation_cleanup_failure(&plan, &roots, &[0]).is_none());
        assert!(source.exists());
        assert!(!destination.exists());
        assert!(!plan.steps[0].destination.as_ref().unwrap().exists());
    }

    #[test]
    fn late_cancellation_preserves_a_complete_filesystem_commit() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"data").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source: source.clone(),
            destination: destination.clone(),
            bytes: 4,
            available_bytes: None,
            dry_run: false,
        });
        std::fs::rename(&source, &destination).unwrap();
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "late-cancel-complete".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "running".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 1,
                done: 1,
                checkpoint: 1,
                file_index: Some(1),
                piece_index: None,
                byte_offset: Some(4),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: Some(now),
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let completion = interrupted_before_start_completion(
            &db,
            "late-cancel-complete",
            "move",
            &plan,
            std::slice::from_ref(&root.path().to_path_buf()),
            &[0],
            false,
        );

        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(completion.succeeded);
        assert_eq!(completion.completed_byte_offset, Some(4));
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), "late-cancel-complete")
                .unwrap()
                .state,
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
        assert!(!source.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"data");
    }

    #[test]
    fn partial_destructive_storage_progress_requires_manual_recovery() {
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: vec![
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::CopyVerifyRename,
                    source: None,
                    destination: None,
                    bytes: 0,
                },
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::Rename,
                    source: None,
                    destination: None,
                    bytes: 0,
                },
                StoragePlanStep {
                    action: rt_storage::PlannedStorageAction::PruneEmptyDirs,
                    source: None,
                    destination: None,
                    bytes: 0,
                },
            ],
            rollback_steps: Vec::new(),
        };

        assert!(!storage_plan_failure_requires_manual_recovery(
            &plan,
            &[0],
            None,
        ));
        assert!(storage_plan_failure_requires_manual_recovery(
            &plan,
            &[0, 1],
            None,
        ));
        assert!(storage_plan_failure_requires_manual_recovery(
            &plan,
            &[2],
            None,
        ));
        assert!(storage_plan_failure_requires_manual_recovery(
            &plan,
            &[],
            Some(&StorageError::FilesystemStateUncertain {
                step: "reconcile",
                reason: "ambiguous state".to_owned(),
            }),
        ));
        assert!(storage_plan_failure_requires_manual_recovery(
            &plan,
            &[],
            Some(&StorageError::StagedMoveFailed {
                step: "checkpoint",
                reason: "durable checkpoint failed".to_owned(),
            }),
        ));
    }

    #[tokio::test]
    async fn dispatcher_rejects_duplicate_active_job_ids() {
        let executor: StorageJobExecutor = Arc::new(|_| StorageJobCompletion {
            succeeded: true,
            state: "completed".to_owned(),
            error: None,
            completed_steps: Vec::new(),
            completed_byte_offset: None,
            requires_manual_recovery: false,
        });
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let (first_completion, _first_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "duplicate-job".to_owned(),
                "import".to_owned(),
                plan.clone(),
                Vec::new(),
                Vec::new(),
                first_completion,
            )
            .unwrap();
        let (second_completion, _second_rx) = oneshot::channel();
        let error = dispatcher
            .submit(
                db,
                "duplicate-job".to_owned(),
                "import".to_owned(),
                plan,
                Vec::new(),
                Vec::new(),
                second_completion,
            )
            .unwrap_err();
        assert_eq!(error, "storage job duplicate-job is already active");
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn dispatcher_rejects_when_end_to_end_inflight_bound_is_reached() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        for job_id in ["paused-one", "paused-two", "paused-three"] {
            rt_db::upsert_job(
                &connection,
                &rt_db::JobRow {
                    job_id: job_id.to_owned(),
                    kind: "storage_plan".to_owned(),
                    state: "paused".to_owned(),
                    dry_run: false,
                    affected_torrents: Vec::new(),
                    total: 0,
                    done: 0,
                    checkpoint: 0,
                    file_index: Some(0),
                    piece_index: None,
                    byte_offset: Some(0),
                    verified_bytes: 0,
                    invalid_pieces: Vec::new(),
                    error: None,
                    created_at: now,
                    started_at: None,
                    updated_at: now,
                    finished_at: None,
                },
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(connection));
        let mut dispatcher = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 2);
        let root = tempfile::tempdir().unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };

        {
            let job_id = "paused-one";
            let (completion, _completion_rx) = oneshot::channel();
            dispatcher
                .submit_paused(
                    Arc::clone(&db),
                    job_id.to_owned(),
                    "move".to_owned(),
                    plan.clone(),
                    Vec::new(),
                    vec![root.path().to_path_buf()],
                    completion,
                )
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        for job_id in ["paused-two", "paused-three"] {
            let (completion, _completion_rx) = oneshot::channel();
            dispatcher
                .submit_paused(
                    Arc::clone(&db),
                    job_id.to_owned(),
                    "move".to_owned(),
                    plan.clone(),
                    Vec::new(),
                    vec![root.path().to_path_buf()],
                    completion,
                )
                .unwrap();
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
        let stats = dispatcher.stats();
        assert_eq!(stats.inflight, 3);
        assert_eq!(stats.capacity, 3);
        assert_eq!(stats.worker_count, 1);

        let (completion, _completion_rx) = oneshot::channel();
        let error = dispatcher
            .submit_paused(
                Arc::clone(&db),
                "paused-four".to_owned(),
                "move".to_owned(),
                plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap_err();
        assert_eq!(error, "storage job dispatcher is at capacity");
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn closed_worker_releases_end_to_end_inflight_registration() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let db = Arc::new(Mutex::new(connection));
        let control = Arc::new(StorageJobControl::new(false));
        let controls = Arc::new(Mutex::new(HashMap::from([(
            "panic-job".to_owned(),
            Arc::clone(&control),
        )])));
        let (completion, _completion_rx) = oneshot::channel();
        let request = StorageJobRequest {
            job_id: "panic-job".to_owned(),
            operation: "move".to_owned(),
            plan: StoragePlan {
                dry_run: false,
                can_apply: true,
                issues: Vec::new(),
                steps: Vec::new(),
                rollback_steps: Vec::new(),
            },
            completed_steps: Vec::new(),
            roots: Vec::new(),
            db,
            control,
            completion,
        };
        let slots = Arc::new(Semaphore::new(0));
        slots.close();

        let result = tokio::spawn(run_storage_request(
            request,
            slots,
            Arc::clone(&controls),
            Arc::new(execute_storage_job),
        ))
        .await
        .unwrap();
        assert_eq!(result, "panic-job");
        assert!(controls.lock().unwrap().is_empty());
    }

    #[test]
    fn worker_registration_guard_releases_on_unwind() {
        let controls = Arc::new(Mutex::new(HashMap::from([(
            "panic-job".to_owned(),
            Arc::new(StorageJobControl::new(false)),
        )])));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let controls = Arc::clone(&controls);
            move || {
                let _registration = StorageJobRegistration {
                    job_id: "panic-job".to_owned(),
                    controls,
                };
                panic!("test worker panic");
            }
        }));
        assert!(result.is_err());
        assert!(controls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn injected_worker_panic_is_contained_and_next_job_runs() {
        let root = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        for job_id in ["panic-storage", "healthy-storage"] {
            rt_db::upsert_job(
                &connection,
                &rt_db::JobRow {
                    job_id: job_id.to_owned(),
                    kind: "storage_plan".to_owned(),
                    state: "queued".to_owned(),
                    dry_run: false,
                    affected_torrents: Vec::new(),
                    total: 0,
                    done: 0,
                    checkpoint: 0,
                    file_index: Some(0),
                    piece_index: None,
                    byte_offset: Some(0),
                    verified_bytes: 0,
                    invalid_pieces: Vec::new(),
                    error: None,
                    created_at: now,
                    started_at: None,
                    updated_at: now,
                    finished_at: None,
                },
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(connection));
        let executor: StorageJobExecutor = Arc::new(|execution| {
            if execution.job_id == "panic-storage" {
                panic!("injected storage worker panic");
            }
            execute_storage_job(execution)
        });
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };

        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "panic-storage".to_owned(),
                "move".to_owned(),
                plan.clone(),
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!completion.succeeded);
        assert_eq!(completion.state, "failed");
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("panicked")));
        for _ in 0..10 {
            if dispatcher.stats().inflight == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(dispatcher.is_healthy());

        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "healthy-storage".to_owned(),
                "move".to_owned(),
                plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(completion.succeeded);
        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(dispatcher.is_healthy());
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn shutdown_requeues_active_and_queued_jobs_for_restart() {
        let root = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        for job_id in ["active-before-shutdown", "queued-before-shutdown"] {
            rt_db::upsert_job(
                &connection,
                &rt_db::JobRow {
                    job_id: job_id.to_owned(),
                    kind: "storage_plan".to_owned(),
                    state: "queued".to_owned(),
                    dry_run: false,
                    affected_torrents: Vec::new(),
                    total: 0,
                    done: 0,
                    checkpoint: 0,
                    file_index: Some(0),
                    piece_index: None,
                    byte_offset: Some(0),
                    verified_bytes: 0,
                    invalid_pieces: Vec::new(),
                    error: None,
                    created_at: now,
                    started_at: None,
                    updated_at: now,
                    finished_at: None,
                },
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(connection));
        let executor: StorageJobExecutor = Arc::new(|execution| {
            // Hold the blocking worker long enough for shutdown to mark both
            // the running and semaphore-waiting requests as interrupted.
            std::thread::sleep(Duration::from_millis(100));
            execute_storage_job(execution)
        });
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let mut completions = Vec::new();
        for job_id in ["active-before-shutdown", "queued-before-shutdown"] {
            let (completion, completion_rx) = oneshot::channel();
            dispatcher
                .submit(
                    Arc::clone(&db),
                    job_id.to_owned(),
                    "delete".to_owned(),
                    plan.clone(),
                    Vec::new(),
                    vec![root.path().to_path_buf()],
                    completion,
                )
                .unwrap();
            completions.push(completion_rx);
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
        dispatcher.shutdown(Duration::from_secs(1)).await;

        for completion_rx in completions {
            let completion = completion_rx.await.unwrap();
            assert_eq!(completion.state, "queued");
            assert!(!completion.succeeded);
        }
        let db = db.lock().unwrap();
        for job_id in ["active-before-shutdown", "queued-before-shutdown"] {
            let job = rt_db::get_job(&db, job_id).unwrap();
            assert_eq!(job.state, "queued");
            assert!(job.finished_at.is_none());
            assert!(job
                .error
                .as_deref()
                .is_some_and(|error| error.contains("recovered after restart")));
        }
    }

    #[tokio::test]
    async fn shutdown_wakes_requests_waiting_for_a_wedged_worker_slot() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        for job_id in ["shutdown-wedged-worker", "shutdown-slot-waiter"] {
            rt_db::upsert_job(
                &connection,
                &rt_db::JobRow {
                    job_id: job_id.to_owned(),
                    kind: "storage_plan".to_owned(),
                    state: "queued".to_owned(),
                    dry_run: false,
                    affected_torrents: Vec::new(),
                    total: 0,
                    done: 0,
                    checkpoint: 0,
                    file_index: Some(0),
                    piece_index: None,
                    byte_offset: Some(0),
                    verified_bytes: 0,
                    invalid_pieces: Vec::new(),
                    error: None,
                    created_at: now,
                    started_at: None,
                    updated_at: now,
                    finished_at: None,
                },
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(connection));
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let release = Arc::new(Barrier::new(2));
        let executor: StorageJobExecutor = Arc::new({
            let release = Arc::clone(&release);
            move |_execution| {
                started_tx.send(()).unwrap();
                release.wait();
                StorageJobCompletion::requeued("test worker released", Vec::new())
            }
        });
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let (first_completion, _first_completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "shutdown-wedged-worker".to_owned(),
                "delete".to_owned(),
                plan.clone(),
                Vec::new(),
                Vec::new(),
                first_completion,
            )
            .unwrap();
        let (second_completion, second_completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "shutdown-slot-waiter".to_owned(),
                "delete".to_owned(),
                plan,
                Vec::new(),
                Vec::new(),
                second_completion,
            )
            .unwrap();

        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        dispatcher.shutdown(Duration::from_millis(20)).await;

        let second = tokio::time::timeout(Duration::from_secs(1), second_completion_rx)
            .await
            .expect("slot waiter did not receive shutdown requeue")
            .expect("slot waiter completion was dropped");
        assert_eq!(second.state, "queued");
        assert!(!second.succeeded);
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), "shutdown-slot-waiter")
                .unwrap()
                .state,
            "queued"
        );

        let release_waiter = tokio::task::spawn_blocking(move || release.wait());
        release_waiter.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_rejects_submissions_after_supervisor_stops() {
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let mut dispatcher = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 1);
        dispatcher.shutdown(Duration::from_secs(1)).await;

        let (completion, _completion_rx) = oneshot::channel();
        let error = dispatcher
            .submit(
                db,
                "after-shutdown".to_owned(),
                "delete".to_owned(),
                StoragePlan {
                    dry_run: false,
                    can_apply: true,
                    issues: Vec::new(),
                    steps: Vec::new(),
                    rollback_steps: Vec::new(),
                },
                Vec::new(),
                Vec::new(),
                completion,
            )
            .unwrap_err();
        assert_eq!(error, "storage job worker is shutting down");
    }

    #[tokio::test]
    async fn shutdown_requeues_paused_worker_result_after_late_control() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let move_plan = plan_move(&MovePlanRequest {
            source,
            destination,
            bytes: 7,
            available_bytes: None,
            dry_run: false,
        });
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "late-shutdown-pause".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "queued".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 1,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let worker_result_ready = Arc::new(Barrier::new(2));
        let release_worker_result = Arc::new(Barrier::new(2));
        let executor: StorageJobExecutor = {
            let worker_result_ready = Arc::clone(&worker_result_ready);
            let release_worker_result = Arc::clone(&release_worker_result);
            Arc::new(move |execution| {
                // Force execute_storage_job to persist a paused checkpoint,
                // then hold its completion after it has been computed. The
                // test applies shutdown in this gap, matching the supervisor
                // race between the blocking join and its state check.
                execution.control.apply(StorageJobAction::Pause);
                let completion = execute_storage_job(execution);
                assert_eq!(completion.state, "paused");
                worker_result_ready.wait();
                release_worker_result.wait();
                completion
            })
        };
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);
        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "late-shutdown-pause".to_owned(),
                "move".to_owned(),
                move_plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();

        tokio::task::spawn_blocking(move || worker_result_ready.wait())
            .await
            .unwrap();
        dispatcher
            .control("late-shutdown-pause", StorageJobAction::Shutdown)
            .unwrap();
        tokio::task::spawn_blocking(move || release_worker_result.wait())
            .await
            .unwrap();

        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.state, "queued");
        assert!(!completion.succeeded);
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("recovered after restart")));
        {
            let db = db.lock().unwrap();
            let job = rt_db::get_job(&db, "late-shutdown-pause").unwrap();
            assert_eq!(job.state, "queued");
            assert!(job.finished_at.is_none());
            assert_eq!(job.done, 0);
        }
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn paused_job_waits_without_consuming_worker_slot_until_resumed() {
        let root = tempfile::tempdir().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "paused-storage".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "paused".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let mut dispatcher = StorageJobDispatcher::with_limits(Arc::clone(&db), 1, 1);
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let (completion, mut completion_rx) = oneshot::channel();
        dispatcher
            .submit_paused(
                Arc::clone(&db),
                "paused-storage".to_owned(),
                "move".to_owned(),
                plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();

        tokio::time::sleep(Duration::from_millis(40)).await;
        {
            let conn = db.lock().unwrap();
            assert_eq!(
                rt_db::get_job(&conn, "paused-storage").unwrap().state,
                "paused"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut completion_rx)
                .await
                .is_err()
        );

        dispatcher
            .control("paused-storage", StorageJobAction::Resume)
            .unwrap();
        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(completion.succeeded);
        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(completion.error.is_none());
        assert!(completion.completed_steps.is_empty());
        {
            let conn = db.lock().unwrap();
            assert_eq!(
                rt_db::get_job(&conn, "paused-storage").unwrap().state,
                STORAGE_JOB_STATE_COMMIT_PENDING
            );
        }
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn pause_at_step_boundary_releases_worker_for_another_job() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let move_plan = plan_move(&MovePlanRequest {
            source,
            destination,
            bytes: 7,
            available_bytes: None,
            dry_run: false,
        });
        let empty_plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        for (job_id, state) in [("pause-mid-step", "queued"), ("unrelated-job", "queued")] {
            rt_db::upsert_job(
                &connection,
                &rt_db::JobRow {
                    job_id: job_id.to_owned(),
                    kind: "storage_plan".to_owned(),
                    state: state.to_owned(),
                    dry_run: false,
                    affected_torrents: Vec::new(),
                    total: 1,
                    done: 0,
                    checkpoint: 0,
                    file_index: Some(0),
                    piece_index: None,
                    byte_offset: Some(0),
                    verified_bytes: 0,
                    invalid_pieces: Vec::new(),
                    error: None,
                    created_at: now,
                    started_at: None,
                    updated_at: now,
                    finished_at: None,
                },
            )
            .unwrap();
        }
        let db = Arc::new(Mutex::new(connection));
        let pause_once = Arc::new(AtomicBool::new(true));
        let executor: StorageJobExecutor = {
            let pause_once = Arc::clone(&pause_once);
            Arc::new(move |execution| {
                if execution.job_id == "pause-mid-step" && pause_once.swap(false, Ordering::AcqRel)
                {
                    // This happens before the real filesystem step. The
                    // executor must observe it at the checkpoint *after* the
                    // rename, which makes the test cover the expensive,
                    // blocking boundary rather than the pre-start path.
                    execution.control.apply(StorageJobAction::Pause);
                }
                execute_storage_job(execution)
            })
        };
        let mut dispatcher = StorageJobDispatcher::with_test_executor(1, 1, executor);

        let (paused_completion, mut paused_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "pause-mid-step".to_owned(),
                "move".to_owned(),
                move_plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                paused_completion,
            )
            .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if rt_db::get_job(&db.lock().unwrap(), "pause-mid-step")
                    .unwrap()
                    .state
                    == "paused"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("pause-at-step-boundary job did not reach paused state");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut paused_rx)
                .await
                .is_err()
        );

        let (other_completion, other_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "unrelated-job".to_owned(),
                "delete".to_owned(),
                empty_plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                other_completion,
            )
            .unwrap();
        let other = tokio::time::timeout(Duration::from_secs(1), other_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(other.state, "completed");
        assert!(other.succeeded);
        assert!(other.error.is_none());

        dispatcher
            .control("pause-mid-step", StorageJobAction::Resume)
            .unwrap();
        let paused = tokio::time::timeout(Duration::from_secs(1), &mut paused_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(paused.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(paused.succeeded);
        assert_eq!(paused.completed_steps, vec![0]);
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn production_dispatcher_uses_dedicated_database_connection() {
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("state.db");
        let connection = Connection::open(&db_path).unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "dedicated-db-job".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "queued".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let mut dispatcher = StorageJobDispatcher::new(&db_path).unwrap();
        let root = tempfile::tempdir().unwrap();
        let plan = StoragePlan {
            dry_run: false,
            can_apply: true,
            issues: Vec::new(),
            steps: Vec::new(),
            rollback_steps: Vec::new(),
        };
        let (completion, completion_rx) = oneshot::channel();
        dispatcher
            .submit(
                Arc::clone(&db),
                "dedicated-db-job".to_owned(),
                "move".to_owned(),
                plan,
                Vec::new(),
                vec![root.path().to_path_buf()],
                completion,
            )
            .unwrap();

        let completion = tokio::time::timeout(Duration::from_secs(1), completion_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(completion.succeeded);
        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert_eq!(
            rt_db::get_job(&db.lock().unwrap(), "dedicated-db-job")
                .unwrap()
                .state,
            STORAGE_JOB_STATE_COMMIT_PENDING
        );
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[test]
    fn terminal_persistence_records_partial_progress() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let db = Arc::new(Mutex::new(connection));
        let now = unix_now_i64();
        let job = rt_db::JobRow {
            job_id: "storage-test".to_owned(),
            kind: "storage_plan".to_owned(),
            state: "queued".to_owned(),
            dry_run: false,
            affected_torrents: Vec::new(),
            total: 1,
            done: 0,
            checkpoint: 0,
            file_index: Some(0),
            piece_index: None,
            byte_offset: Some(0),
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: now,
            started_at: None,
            updated_at: now,
            finished_at: None,
        };
        {
            let conn = db.lock().unwrap();
            rt_db::upsert_job(&conn, &job).unwrap();
        }
        let plan = plan_move(&MovePlanRequest {
            source: PathBuf::from("/source"),
            destination: PathBuf::from("/destination"),
            bytes: 1,
            available_bytes: None,
            dry_run: false,
        });
        persist_terminal(
            &db,
            "storage-test",
            "move",
            &plan,
            &[0],
            "failed",
            Some("test failure".to_owned()),
        )
        .unwrap();
        let conn = db.lock().unwrap();
        let row = rt_db::get_job(&conn, "storage-test").unwrap();
        assert_eq!(row.state, "failed");
        assert_eq!(row.done, 1);
        assert_eq!(row.file_index, Some(1));
        assert_eq!(row.byte_offset, Some(1));
        assert!(row.finished_at.is_some());
    }

    #[test]
    fn running_persistence_failure_records_terminal_job_state() {
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "running-persistence-failure".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "queued".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_running_event
                 BEFORE INSERT ON job_events
                 WHEN NEW.kind = 'storage_plan_running'
                 BEGIN SELECT RAISE(ABORT, 'injected running persistence failure'); END;",
            )
            .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let root = tempfile::tempdir().unwrap();
        let completion = execute_storage_job(StorageJobExecution {
            job_id: "running-persistence-failure".to_owned(),
            operation: "move".to_owned(),
            plan: StoragePlan {
                dry_run: false,
                can_apply: true,
                issues: Vec::new(),
                steps: Vec::new(),
                rollback_steps: Vec::new(),
            },
            completed_steps: Vec::new(),
            roots: vec![root.path().to_path_buf()],
            db: Arc::clone(&db),
            control: Arc::new(StorageJobControl::new(false)),
        });

        assert!(!completion.succeeded);
        assert_eq!(completion.state, "failed");
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("running persistence")));
        let job = rt_db::get_job(&db.lock().unwrap(), "running-persistence-failure").unwrap();
        assert_eq!(job.state, "failed");
        assert!(job.finished_at.is_some());
    }

    #[test]
    fn move_persistence_failure_remains_commit_pending() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.bin");
        let destination = root.path().join("destination.bin");
        std::fs::write(&source, b"payload").unwrap();
        let plan = plan_move(&MovePlanRequest {
            source,
            destination,
            bytes: 7,
            available_bytes: None,
            dry_run: false,
        });
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection).unwrap();
        let now = unix_now_i64();
        rt_db::upsert_job(
            &connection,
            &rt_db::JobRow {
                job_id: "move-persistence-failure".to_owned(),
                kind: "storage_plan".to_owned(),
                state: "queued".to_owned(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: i64::try_from(plan.steps.len()).unwrap(),
                done: 0,
                checkpoint: 0,
                file_index: Some(0),
                piece_index: None,
                byte_offset: Some(0),
                verified_bytes: 0,
                invalid_pieces: Vec::new(),
                error: None,
                created_at: now,
                started_at: None,
                updated_at: now,
                finished_at: None,
            },
        )
        .unwrap();
        let db = Arc::new(Mutex::new(connection));
        db.lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_terminal_event
             BEFORE INSERT ON job_events
             WHEN NEW.kind = 'storage_plan_commit_pending'
             BEGIN SELECT RAISE(ABORT, 'injected terminal persistence failure'); END;",
            )
            .unwrap();
        let completion = execute_storage_job(StorageJobExecution {
            job_id: "move-persistence-failure".to_owned(),
            operation: "move".to_owned(),
            plan,
            completed_steps: Vec::new(),
            roots: vec![root.path().to_path_buf()],
            db: Arc::clone(&db),
            control: Arc::new(StorageJobControl::new(false)),
        });

        assert!(completion.succeeded);
        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminal persistence failed")));
        assert_eq!(completion.completed_steps, vec![0]);
        assert_eq!(completion.completed_byte_offset, Some(7));
        let conn = db.lock().unwrap();
        let job = rt_db::get_job(&conn, "move-persistence-failure").unwrap();
        assert_eq!(job.state, "running");
        assert_eq!(job.byte_offset, Some(7));
    }
}
