//! Bounded, durable storage-plan execution outside the engine actor.
//!
//! The engine actor owns orchestration and command ordering. It must not own
//! filesystem copies, renames, deletes, or long-running verification. This
//! module is the narrow worker seam: plans are queued with a bounded channel,
//! executed on blocking threads behind a fixed semaphore, and checkpointed in
//! the same SQLite database used by the actor.

use std::collections::HashMap;
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
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageJobAction {
    Pause,
    Resume,
    Cancel,
}

struct StorageJobControl {
    paused: AtomicBool,
    cancelled: AtomicBool,
    notify: Notify,
}

impl StorageJobControl {
    fn new(paused: bool) -> Self {
        Self {
            paused: AtomicBool::new(paused),
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn apply(self: &Arc<Self>, action: StorageJobAction) {
        match action {
            StorageJobAction::Pause => self.paused.store(true, Ordering::Release),
            StorageJobAction::Resume => self.paused.store(false, Ordering::Release),
            StorageJobAction::Cancel => self.cancelled.store(true, Ordering::Release),
        }
        self.notify.notify_waiters();
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
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

    fn wait_if_paused(&self) -> Result<(), StorageError> {
        while self.paused.load(Ordering::Acquire) {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(cancelled_error());
            }
            std::thread::sleep(PAUSE_POLL_INTERVAL);
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
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

pub(crate) struct StorageJobCompletion {
    pub(crate) succeeded: bool,
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
    pub(crate) fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self::with_limits(db, DEFAULT_WORKER_COUNT, DEFAULT_QUEUE_CAPACITY)
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            controls: Arc::new(Mutex::new(HashMap::new())),
            max_inflight: 2,
            worker_count: 1,
            shutdown_tx: None,
            join: None,
        }
    }

    pub(crate) fn with_limits(
        db: Arc<Mutex<Connection>>,
        worker_count: usize,
        queue_capacity: usize,
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
        let join = tokio::spawn(run_supervisor(rx, shutdown_rx, slots, supervisor_controls));
        // `db` is carried in each request so the dispatcher remains a pure
        // execution boundary and can be tested with isolated in-memory DBs.
        drop(db);
        Self {
            tx,
            controls,
            max_inflight,
            worker_count,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }

    // Single call site (the engine actor submitting a storage-plan job to
    // the background worker); each parameter is a distinct piece of state
    // the worker needs, not a natural grouping.
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
        let control = Arc::new(StorageJobControl::new(paused));
        {
            let mut controls = self
                .controls
                .lock()
                .expect("storage job controls mutex poisoned");
            if controls.len() >= self.max_inflight {
                return Err("storage job dispatcher is at capacity".to_owned());
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
                    control.apply(StorageJobAction::Cancel);
                }
                break;
            },
            Some(request) = rx.recv() => {
                let slots = Arc::clone(&slots);
                let request_controls = Arc::clone(&controls);
                active.spawn(async move {
                    run_storage_request(request, slots, request_controls).await
                });
            }
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

    // Requests still in the bounded queue never started. Mark them as
    // cancelled instead of leaving durable jobs indefinitely in `queued`.
    while let Ok(request) = rx.try_recv() {
        let _ = persist_terminal(
            &request.db,
            &request.job_id,
            &request.operation,
            &request.plan,
            &request.completed_steps,
            "cancelled",
            Some("storage worker shutdown".to_owned()),
        );
        let _ = request
            .completion
            .send(StorageJobCompletion { succeeded: false });
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
) -> String {
    let job_id = request.job_id.clone();
    let _registration = StorageJobRegistration {
        job_id: job_id.clone(),
        controls,
    };
    let control = Arc::clone(&request.control);
    loop {
        control.wait_until_runnable().await;
        let recovery_db = Arc::clone(&request.db);
        let recovery_operation = request.operation.clone();
        let recovery_plan = request.plan.clone();
        let recovery_completed_steps = request.completed_steps.clone();
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
                    Some(reason),
                );
                let _ = request
                    .completion
                    .send(StorageJobCompletion { succeeded: false });
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
        let result = tokio::task::spawn_blocking(move || execute_storage_job(request)).await;
        drop(slot);
        if let Err(error) = result {
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
        }
        return job_id;
    }
}

fn execute_storage_job(request: StorageJobRequest) {
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
    let mut completed = completed_steps;
    let already_completed = completed.clone();
    let _ = persist_running(&db, &job_id);

    let checkpoint = |index: usize, _step: &StoragePlanStep| {
        control.wait_if_paused()?;
        if !completed.contains(&index) {
            completed.push(index);
            completed.sort_unstable();
        }
        persist_checkpoint(&db, &job_id, &operation, &plan, &completed).map_err(|error| {
            StorageError::StagedMoveFailed {
                step: "checkpoint",
                reason: error,
            }
        })
    };
    let result = if control.cancelled.load(Ordering::Acquire) {
        Err(cancelled_error())
    } else {
        rt_storage::execute_storage_plan_under_roots_with_checkpoints(
            &plan,
            &roots,
            &already_completed,
            checkpoint,
        )
    };
    let cancelled = control.cancelled.load(Ordering::Acquire);
    let terminal_state = if cancelled {
        "cancelled"
    } else if result.is_ok() {
        "completed"
    } else {
        "failed"
    };
    let error = result.err().map(|error| error.to_string());
    if let Err(persist_error) = persist_terminal(
        &db,
        &job_id,
        &operation,
        &plan,
        &completed,
        terminal_state,
        error,
    ) {
        warn!(
            component = "storage_jobs",
            operation = "persist_terminal",
            job_id = %job_id,
            result = "error",
            error = %persist_error,
            "failed to persist storage job terminal state"
        );
    }
    let _ = completion.send(StorageJobCompletion {
        succeeded: terminal_state == "completed",
    });
    debug!(
        component = "storage_jobs",
        operation = "execute",
        job_id = %job_id,
        state = terminal_state,
        done = completed.len(),
        "storage plan worker finished"
    );
}

fn cancelled_error() -> StorageError {
    StorageError::StagedMoveFailed {
        step: "cancel",
        reason: "storage plan cancelled".to_owned(),
    }
}

fn persist_running(db: &Arc<Mutex<Connection>>, job_id: &str) -> Result<(), String> {
    let mut job = load_job(db, job_id)?;
    job.state = "running".to_owned();
    if job.started_at.is_none() {
        job.started_at = Some(unix_now_i64());
    }
    job.updated_at = unix_now_i64();
    let conn = db.lock().expect("database mutex poisoned");
    rt_db::upsert_job(&conn, &job).map_err(|error| error.to_string())
}

fn persist_checkpoint(
    db: &Arc<Mutex<Connection>>,
    job_id: &str,
    operation: &str,
    plan: &StoragePlan,
    completed_steps: &[usize],
) -> Result<(), String> {
    let mut job = load_job(db, job_id)?;
    job.done = completed_steps.len() as i64;
    job.checkpoint = job.done;
    job.file_index = Some(job.done);
    job.byte_offset = Some(
        completed_steps
            .iter()
            .filter_map(|index| plan.steps.get(*index))
            .map(|step| step.bytes as i64)
            .sum(),
    );
    job.updated_at = unix_now_i64();
    let event = rt_db::JobEventRow {
        event_id: None,
        job_id: job_id.to_owned(),
        occurred_at: job.updated_at,
        kind: "storage_plan_checkpoint".to_owned(),
        message: Some("storage plan checkpoint persisted".to_owned()),
        payload: storage_plan_payload(operation, plan, completed_steps).to_string(),
    };
    let conn = db.lock().expect("database mutex poisoned");
    rt_db::upsert_job(&conn, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event(&conn, &event).map_err(|error| error.to_string())?;
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
    job.done = completed_steps.len() as i64;
    job.checkpoint = job.done;
    job.error = error.clone();
    job.updated_at = unix_now_i64();
    job.finished_at = Some(job.updated_at);
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
    let conn = db.lock().expect("database mutex poisoned");
    rt_db::upsert_job(&conn, &job).map_err(|error| error.to_string())?;
    rt_db::append_job_event(&conn, &event).map_err(|error| error.to_string())?;
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

        let result = tokio::spawn(run_storage_request(request, slots, Arc::clone(&controls)))
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
        {
            let conn = db.lock().unwrap();
            assert_eq!(
                rt_db::get_job(&conn, "paused-storage").unwrap().state,
                "completed"
            );
        }
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
}
