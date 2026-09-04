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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot, Notify, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tracing::{debug, warn};

use rt_storage::{StorageError, StoragePlan, StoragePlanStep};

use crate::engine::unix_now_i64;

const DEFAULT_QUEUE_CAPACITY: usize = 32;
const DEFAULT_WORKER_COUNT: usize = 2;
pub(crate) const STORAGE_JOB_STATE_COMMIT_PENDING: &str = "commit_pending";

fn db_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn db_i64_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn completed_byte_offset(plan: &StoragePlan, completed_steps: &[usize]) -> i64 {
    completed_steps
        .iter()
        .filter_map(|index| plan.steps.get(*index))
        .map(|step| db_i64(step.bytes))
        .fold(0_i64, i64::saturating_add)
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
                self.cancelled.store(true, Ordering::Release);
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
            if !self.is_paused() || self.cancelled.load(Ordering::Acquire) {
                return;
            }
            let notified = self.notify.notified();
            if !self.is_paused() || self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Check control state from a blocking filesystem worker. A pause is not
    /// waited out on the worker: doing that would consume a worker slot for
    /// the entire pause and starve unrelated storage jobs. The supervisor
    /// persists the completed step, releases the slot, and waits
    /// asynchronously before reattaching this job on resume.
    fn check_step_boundary(&self) -> Result<(), StorageError> {
        // Deliberate, opt-in fault-injection hook for the live acceptance
        // harness. It gives an API cancellation request a deterministic
        // window before the first filesystem mutation. The default is zero;
        // ordinary deployments never sleep here.
        if self.fault_delay_ms > 0 && !self.fault_delay_consumed.swap(true, Ordering::AcqRel) {
            std::thread::sleep(Duration::from_millis(self.fault_delay_ms));
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
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
        }
    }

    pub(crate) fn failed(error: impl Into<String>, completed_steps: Vec<usize>) -> Self {
        Self::terminal("failed", Some(error.into()), completed_steps)
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
        if let Ok(mut controls) = self.controls.lock() {
            controls.remove(&self.job_id);
        }
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
    ) -> Result<(), String> {
        let db = self.worker_db.as_ref().cloned().unwrap_or(db);
        let control = Arc::new(StorageJobControl::new(paused));
        {
            let mut controls = self
                .controls
                .lock()
                .expect("storage job controls mutex poisoned");
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
            self.controls
                .lock()
                .expect("storage job controls mutex poisoned")
                .remove(&job_id);
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
        let control = self
            .controls
            .lock()
            .expect("storage job controls mutex poisoned")
            .get(job_id)
            .cloned()
            .ok_or_else(|| format!("storage worker has no active job {job_id}"))?;
        control.apply(action);
        Ok(())
    }

    pub(crate) fn stats(&self) -> StorageJobStats {
        let inflight = self
            .controls
            .lock()
            .expect("storage job controls mutex poisoned")
            .len();
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
        let Some(mut join) = self.join.take() else {
            return;
        };
        if timeout(timeout_budget, &mut join).await.is_err() {
            warn!(
                component = "storage_jobs",
                operation = "shutdown",
                result = "timeout",
                "storage workers did not drain before shutdown deadline"
            );
            join.abort();
        }
    }
}

impl Drop for StorageJobDispatcher {
    fn drop(&mut self) {
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
                let active_controls = controls
                    .lock()
                    .expect("storage job controls mutex poisoned")
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
        for control in active_controls {
                    control.apply(StorageJobAction::Shutdown);
                }
                break;
            },
            Some(result) = active.join_next(), if !active.is_empty() => {
                match result {
                    Ok(job_id) => {
                        controls
                            .lock()
                            .expect("storage job controls mutex poisoned")
                            .remove(&job_id);
                    }
                    Err(error) => {
                        warn!(component = "storage_jobs", operation = "join", result = "error", error = %error, "storage worker join failed");
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
                controls
                    .lock()
                    .expect("storage job controls mutex poisoned")
                    .remove(&job_id);
            }
            Err(error) => {
                warn!(component = "storage_jobs", operation = "join", result = "error", error = %error, "storage worker join failed during drain");
            }
        }
    }

    // Requests still in the bounded queue never started. Keep them queued so
    // a process restart can reattach and execute them; reporting them as
    // terminally cancelled would strand durable work (especially delete jobs).
    while let Ok(request) = rx.try_recv() {
        let completed_steps = request.completed_steps.clone();
        let _ = persist_requeued(
            &request.db,
            &request.job_id,
            &request.operation,
            &request.plan,
            &request.completed_steps,
            "storage worker shutdown; job will be recovered after restart",
        );
        let _ = request.completion.send(StorageJobCompletion::requeued(
            "storage worker shutdown; job will be recovered after restart",
            completed_steps,
        ));
        controls
            .lock()
            .expect("storage job controls mutex poisoned")
            .remove(&request.job_id);
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
        let recovery_db = Arc::clone(&db);
        let recovery_operation = operation.clone();
        let recovery_plan = plan.clone();
        let recovery_completed_steps = completed_steps.clone();
        let slot = match Arc::clone(&slots).acquire_owned().await {
            Ok(slot) => slot,
            Err(error) => {
                let reason = format!("storage worker semaphore closed: {error}");
                let _ = persist_terminal(
                    &recovery_db,
                    &job_id,
                    &recovery_operation,
                    &recovery_plan,
                    &recovery_completed_steps,
                    "failed",
                    Some(reason.clone()),
                );
                let _ = completion.send(StorageJobCompletion::failed(
                    reason,
                    recovery_completed_steps,
                ));
                return job_id;
            }
        };
        // A pause can arrive after the wait and before the slot is acquired.
        // Release the slot and wait again so paused jobs do not starve active
        // work behind the fixed worker budget.
        if control.is_paused() && !control.cancelled.load(Ordering::Acquire) {
            drop(slot);
            continue;
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
                let reason = format!("storage worker panicked: {error}");
                let _ = persist_terminal(
                    &recovery_db,
                    &job_id,
                    &recovery_operation,
                    &recovery_plan,
                    &recovery_completed_steps,
                    "failed",
                    Some(reason.clone()),
                );
                warn!(
                    component = "storage_jobs",
                    operation = "worker",
                    job_id = %job_id,
                    result = "panic",
                    error = %error,
                    "storage worker task failed; durable job marked failed"
                );
                StorageJobCompletion::failed(reason, recovery_completed_steps)
            }
        };
        if completion_result.state == "paused" && !control.cancelled.load(Ordering::Acquire) {
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
    // Filesystem reconciliation is deliberately inside the blocking worker.
    // It may stat many plan paths and must not run in the engine actor before
    // a request is admitted to the detached storage queue.
    let mut completed =
        match rt_storage::reconcile_storage_plan_under_roots(&plan, &roots, &completed_steps) {
            Ok(completed) => completed,
            Err(error) => {
                let reason = format!("storage plan checkpoint reconciliation failed: {error}");
                let _ = persist_terminal(
                    &db,
                    &job_id,
                    &operation,
                    &plan,
                    &completed_steps,
                    "failed",
                    Some(reason.clone()),
                );
                return StorageJobCompletion::failed(reason, completed_steps);
            }
        };
    let already_completed = completed.clone();
    if let Err(error) = persist_running(&db, &job_id) {
        warn!(
            component = "storage_jobs",
            operation = "persist_running",
            job_id = %job_id,
            result = "error",
            error = %error,
            "storage job did not start because its durable running state could not be written"
        );
        return StorageJobCompletion::failed(
            format!("failed to persist storage job running state: {error}"),
            completed,
        );
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
    let cancelled = control.cancelled.load(Ordering::Acquire) && result.is_err();
    let shutdown_requested = control.is_shutdown();
    let paused = !shutdown_requested
        && !cancelled
        && result.as_ref().is_err_and(|error| {
            matches!(error, StorageError::StagedMoveFailed { step: "pause", .. })
        });
    let terminal_state = if shutdown_requested {
        "queued"
    } else if cancelled {
        "cancelled"
    } else if paused {
        "paused"
    } else if result.is_ok() {
        // A save-path move has one more durable commit after the filesystem
        // transaction: the engine must publish the new path to the torrent
        // row and its live registry projection. Keep the job active until
        // that actor-boundary commit is acknowledged so a crash between the
        // worker and the engine can recover it.
        if operation == "move" {
            STORAGE_JOB_STATE_COMMIT_PENDING
        } else {
            "completed"
        }
    } else {
        "failed"
    };
    let error = result.err().map(|error| error.to_string());
    let persisted_error = paused.then_some("storage plan paused at a step boundary".to_owned());
    let mut completion_state = terminal_state.to_owned();
    let mut completion_error = if paused { None } else { error.clone() };
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
            persisted_error.or(error),
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
        } else if terminal_state == STORAGE_JOB_STATE_COMMIT_PENDING {
            STORAGE_JOB_STATE_COMMIT_PENDING.to_owned()
        } else {
            "failed".to_owned()
        };
        completion_error = Some(match completion_error {
            Some(error) => format!("{error}; terminal persistence failed: {persist_error}"),
            None => format!("terminal persistence failed: {persist_error}"),
        });
    }
    debug!(
        component = "storage_jobs",
        operation = "execute",
        job_id = %job_id,
        state = terminal_state,
        done = completed.len(),
        "storage plan worker finished"
    );
    StorageJobCompletion {
        succeeded: matches!(completion_state.as_str(), STORAGE_JOB_STATE_COMMIT_PENDING)
            || (completion_state == "completed" && completion_error.is_none()),
        state: completion_state,
        error: completion_error,
        completed_steps: completed,
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
    let mut job = load_job(db, job_id)?;
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
        payload: storage_plan_payload(operation, plan, completed_steps).to_string(),
    };
    let mut conn = db.lock().expect("database mutex poisoned");
    let tx = conn.transaction().map_err(|error| error.to_string())?;
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

fn persist_running(db: &Arc<Mutex<Connection>>, job_id: &str) -> Result<(), String> {
    let mut job = load_job(db, job_id)?;
    job.state = "running".to_owned();
    if job.started_at.is_none() {
        job.started_at = Some(unix_now_i64());
    }
    job.updated_at = unix_now_i64();
    let mut conn = db.lock().expect("database mutex poisoned");
    let tx = conn.transaction().map_err(|error| error.to_string())?;
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
    tx.commit().map_err(|error| error.to_string())
}

fn persist_checkpoint(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> Result<(), String> {
    let mut job = load_job(db, job_id)?;
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
        payload: storage_plan_payload(operation, plan, completed_steps).to_string(),
    };
    let mut conn = db.lock().expect("database mutex poisoned");
    let tx = conn.transaction().map_err(|error| error.to_string())?;
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
    let mut job = load_job(db, job_id)?;
    job.state = state.to_owned();
    job.done = db_i64_usize(completed_steps.len());
    job.checkpoint = job.done;
    job.error = error.clone();
    job.updated_at = unix_now_i64();
    job.finished_at = (!matches!(
        state,
        STORAGE_JOB_STATE_COMMIT_PENDING | "paused" | "queued"
    ))
    .then_some(job.updated_at);
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: format!("storage_plan_{state}"),
        message: Some(format!("storage plan {state}")),
        payload: serde_json::json!({
            "error": error,
            "state": state,
            "plan": storage_plan_payload(operation, plan, completed_steps),
        })
        .to_string(),
    };
    let mut conn = db.lock().expect("database mutex poisoned");
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    rt_db::upsert_job_in_tx(&tx, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event_in_tx(&tx, &event).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

fn load_job(db: &Arc<Mutex<Connection>>, job_id: &str) -> Result<rt_db::JobRow, String> {
    let conn = db.lock().expect("database mutex poisoned");
    rt_db::get_job(&conn, job_id).map_err(|error| error.to_string())
}

fn storage_plan_payload(
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> serde_json::Value {
    serde_json::json!({
        "operation": operation,
        "plan": plan,
        "dry_run": plan.dry_run,
        "can_apply": plan.can_apply,
        "completed_steps": completed_steps,
        "steps": plan.steps.iter().map(storage_plan_step_payload).collect::<Vec<_>>(),
        "rollback_steps": plan.rollback_steps.iter().map(storage_plan_step_payload).collect::<Vec<_>>(),
        "issues": plan.issues.iter().map(|issue| format!("{issue:?}")).collect::<Vec<_>>(),
    })
}

fn storage_plan_step_payload(step: &StoragePlanStep) -> serde_json::Value {
    serde_json::json!({
        "action": format!("{:?}", step.action),
        "source": step.source.as_ref().map(|path| path.display().to_string()),
        "destination": step.destination.as_ref().map(|path| path.display().to_string()),
        "bytes": step.bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use rt_db::migrate;
    use rt_storage::{plan_move, MovePlanRequest};

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
        dispatcher.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn dispatcher_rejects_duplicate_active_job_ids() {
        let executor: StorageJobExecutor = Arc::new(|_| StorageJobCompletion {
            succeeded: true,
            state: "completed".to_owned(),
            error: None,
            completed_steps: Vec::new(),
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
        assert!(row.finished_at.is_some());
    }

    #[test]
    fn move_persistence_failure_remains_commit_pending() {
        let root = tempfile::tempdir().unwrap();
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

        assert!(completion.succeeded);
        assert_eq!(completion.state, STORAGE_JOB_STATE_COMMIT_PENDING);
        assert!(completion
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminal persistence failed")));
        let conn = db.lock().unwrap();
        assert_eq!(
            rt_db::get_job(&conn, "move-persistence-failure")
                .unwrap()
                .state,
            "running"
        );
    }
}
