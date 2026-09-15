//! Supervised, bounded execution boundary for engine SQLite work.
//!
//! The engine actor owns ordering and in-memory state. It must not own a
//! SQLite mutex or execute a blocking transaction on its Tokio task. This
//! module provides a bounded command queue and a dedicated blocking thread
//! that owns its SQLite connection. The connection is never shared with the
//! actor, so a blocking query, poisoned state, or operation panic cannot hold
//! the actor's executor hostage.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::Connection;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout, Instant};
use tracing::{debug, warn};

const DB_QUEUE_CAPACITY: usize = 128;
const DB_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const DB_SEND_RETRY: Duration = Duration::from_millis(2);
const DB_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const DB_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on how many `ExecuteBatched` jobs one shared transaction
/// covers. Bounds worst-case commit latency for the job at the front of a
/// very long batch, and bounds how much work a single `catch_unwind`/commit
/// pass does, without capping throughput at realistic per-torrent-tick
/// queue depths (tens of thousands of torrents ticking every few seconds
/// still arrive in small bursts per 50ms worker poll).
const DB_BATCH_MAX: usize = 256;

type ErasedValue = Box<dyn Any + Send>;
type DbOperation = Box<dyn FnOnce(&mut Connection) -> Result<ErasedValue, String> + Send>;
/// A job that runs inside a transaction the worker already opened, shared
/// with other jobs in the same batch — unlike [`DbOperation`], it must not
/// open or commit its own transaction.
type DbTxOperation = Box<dyn FnOnce(&rusqlite::Transaction<'_>) -> Result<ErasedValue, String> + Send>;
type DbResult = Result<ErasedValue, String>;

enum DbReply {
    Async(oneshot::Sender<DbResult>),
    #[cfg(not(test))]
    Blocking(mpsc::SyncSender<DbResult>),
}

enum DbRequest {
    Execute {
        operation: &'static str,
        job: DbOperation,
        cancelled: Arc<AtomicBool>,
        reply: DbReply,
    },
    /// Like `Execute`, but coalescable: the worker may run several queued
    /// `ExecuteBatched` jobs inside one shared transaction instead of one
    /// transaction per job. Used for high-frequency, single-statement,
    /// mutually-independent writes (e.g. per-torrent progress persistence)
    /// where many small independent commits are the actual throughput
    /// ceiling at high torrent counts.
    ExecuteBatched {
        operation: &'static str,
        job: DbTxOperation,
        cancelled: Arc<AtomicBool>,
        reply: DbReply,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

struct CancellationGuard(Arc<AtomicBool>);

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Cloneable handle for the single engine database worker.
#[derive(Clone)]
pub(crate) struct DbWorker {
    tx: mpsc::SyncSender<DbRequest>,
    healthy: Arc<AtomicBool>,
    force_stop: Arc<AtomicBool>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// Database access passed to engine-owned actors.
///
/// Production uses the supervised worker. Tests use their existing in-memory
/// connection fixtures so they can exercise state transitions without
/// changing the production ownership model.
#[derive(Clone)]
pub(crate) enum DbExecutor {
    #[cfg(not(test))]
    Worker(DbWorker),
    #[cfg(test)]
    Direct(Arc<Mutex<Connection>>),
}

impl DbExecutor {
    #[cfg(not(test))]
    pub(crate) fn worker(worker: DbWorker) -> Self {
        Self::Worker(worker)
    }

    #[cfg(test)]
    pub(crate) fn direct(db: Arc<Mutex<Connection>>) -> Self {
        Self::Direct(db)
    }

    pub(crate) async fn run<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        match self {
            #[cfg(not(test))]
            Self::Worker(worker) => worker.run(operation, job).await,
            #[cfg(test)]
            Self::Direct(db) => {
                let mut db = db
                    .lock()
                    .map_err(|_| format!("database mutex poisoned during {operation}"))?;
                job(&mut db)
            }
        }
    }

    /// Like [`run`](Self::run), but the job runs inside a transaction the
    /// executor may share with other queued `run_batched` jobs instead of
    /// opening one transaction per call. The job must not open or commit
    /// its own transaction — it only sees the shared one.
    pub(crate) async fn run_batched<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, String> + Send + 'static,
    {
        match self {
            #[cfg(not(test))]
            Self::Worker(worker) => worker.run_batched(operation, job).await,
            #[cfg(test)]
            Self::Direct(db) => {
                let mut db = db
                    .lock()
                    .map_err(|_| format!("database mutex poisoned during {operation}"))?;
                let tx = db.transaction().map_err(|error| {
                    format!("database worker could not open a transaction: {error}")
                })?;
                let result = job(&tx)?;
                tx.commit()
                    .map_err(|error| format!("batched transaction failed to commit: {error}"))?;
                Ok(result)
            }
        }
    }

    pub(crate) fn run_blocking<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        match self {
            #[cfg(not(test))]
            Self::Worker(worker) => worker.run_blocking(operation, job),
            #[cfg(test)]
            Self::Direct(db) => {
                let mut db = db
                    .lock()
                    .map_err(|_| format!("database mutex poisoned during {operation}"))?;
                job(&mut db)
            }
        }
    }
}

impl DbWorker {
    /// Start a worker whose connection is owned exclusively by its blocking
    /// thread. Opening a second connection is intentional: the session-event
    /// writer and storage supervisor have their own lifecycles, while this
    /// worker is the only authoritative DB boundary used by the engine actor.
    pub(crate) fn new(db_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::sync_channel(DB_QUEUE_CAPACITY);
        let healthy = Arc::new(AtomicBool::new(true));
        let worker_healthy = Arc::clone(&healthy);
        let force_stop = Arc::new(AtomicBool::new(false));
        let worker_force_stop = Arc::clone(&force_stop);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread_builder().spawn(move || {
            let mut db = match Connection::open(&db_path) {
                Ok(db) => db,
                Err(error) => {
                    worker_healthy.store(false, Ordering::Release);
                    warn!(
                        component = "db",
                        operation = "open_worker_connection",
                        result = "error",
                        error = %error,
                        path = %db_path.display(),
                        "engine database worker could not open its connection"
                    );
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            if let Err(error) = db.execute_batch("PRAGMA foreign_keys = ON;") {
                worker_healthy.store(false, Ordering::Release);
                warn!(
                    component = "db",
                    operation = "configure_worker_connection",
                    result = "error",
                    error = %error,
                    "engine database worker could not enable foreign-key enforcement"
                );
                let _ = ready_tx.send(false);
                return;
            }
            // `synchronous` is a per-connection SQLite pragma, unlike
            // `journal_mode = WAL` which is sticky in the database file.
            // rt_db::schema::migrate() only sets it on first-ever creation
            // (schema version 0), so every subsequent process start against
            // an existing database must re-apply it here or this connection
            // silently reverts to SQLite's default `synchronous = FULL`,
            // fsyncing on every commit instead of every WAL checkpoint.
            if let Err(error) = db.execute_batch("PRAGMA synchronous = NORMAL;") {
                worker_healthy.store(false, Ordering::Release);
                warn!(
                    component = "db",
                    operation = "configure_worker_connection",
                    result = "error",
                    error = %error,
                    "engine database worker could not configure synchronous mode"
                );
                let _ = ready_tx.send(false);
                return;
            }
            if let Err(error) = db.busy_timeout(Duration::from_secs(5)) {
                worker_healthy.store(false, Ordering::Release);
                warn!(
                    component = "db",
                    operation = "configure_worker_connection",
                    result = "error",
                    error = %error,
                    "engine database worker could not configure its connection"
                );
                let _ = ready_tx.send(false);
                return;
            }
            let _ = ready_tx.send(true);

            // Holds a request drained from the queue while probing for more
            // `ExecuteBatched` jobs to coalesce, when that request turns out
            // not to belong to the current batch. Processed first on the
            // next iteration instead of being lost or reordered — the same
            // hold-for-next-turn shape used for interrupted lifecycle
            // commands elsewhere in this actor's task loop.
            let mut pending_request: Option<DbRequest> = None;
            loop {
                let request = match pending_request.take() {
                    Some(request) => request,
                    None => match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if worker_force_stop.load(Ordering::Acquire) {
                                break;
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                };
                if worker_force_stop.load(Ordering::Acquire) {
                    break;
                }
                match request {
                    DbRequest::Execute {
                        operation,
                        job,
                        cancelled,
                        reply,
                    } => {
                        // A cancelled actor command may leave its request in
                        // the bounded queue after dropping the oneshot
                        // receiver. Do not apply a stale authoritative write
                        // once nobody can observe its result.
                        if cancelled.load(Ordering::Acquire)
                            || matches!(&reply, DbReply::Async(reply) if reply.is_closed())
                        {
                            continue;
                        }
                        let result = catch_unwind(AssertUnwindSafe(|| job(&mut db)))
                            .map_err(|_| format!("database operation panicked: {operation}"))
                            .and_then(|result| result);
                        if result.is_err() {
                            debug!(
                                component = "db",
                                operation,
                                result = "error",
                                "database operation failed"
                            );
                        }
                        match reply {
                            DbReply::Async(reply) => {
                                let _ = reply.send(result);
                            }
                            #[cfg(not(test))]
                            DbReply::Blocking(reply) => {
                                let _ = reply.send(result);
                            }
                        }
                        if worker_force_stop.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    DbRequest::ExecuteBatched {
                        operation,
                        job,
                        cancelled,
                        reply,
                    } => {
                        let mut batch = vec![(operation, job, cancelled, reply)];
                        while batch.len() < DB_BATCH_MAX {
                            match rx.try_recv() {
                                Ok(DbRequest::ExecuteBatched {
                                    operation,
                                    job,
                                    cancelled,
                                    reply,
                                }) => batch.push((operation, job, cancelled, reply)),
                                Ok(other) => {
                                    pending_request = Some(other);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        match db.transaction() {
                            Ok(tx) => {
                                // Run every job first, but hold each reply
                                // until the shared transaction is known to
                                // have committed — a job must never be told
                                // it succeeded before its data is actually
                                // durable, even though other jobs in the
                                // same batch already ran against `tx`.
                                let mut outcomes: Vec<(DbReply, DbResult)> =
                                    Vec::with_capacity(batch.len());
                                for (operation, job, cancelled, reply) in batch {
                                    if cancelled.load(Ordering::Acquire)
                                        || matches!(&reply, DbReply::Async(r) if r.is_closed())
                                    {
                                        continue;
                                    }
                                    let result = catch_unwind(AssertUnwindSafe(|| job(&tx)))
                                        .map_err(|_| {
                                            format!("database operation panicked: {operation}")
                                        })
                                        .and_then(|result| result);
                                    if result.is_err() {
                                        debug!(
                                            component = "db",
                                            operation,
                                            result = "error",
                                            "database operation failed"
                                        );
                                    }
                                    outcomes.push((reply, result));
                                }
                                let commit_result = tx.commit();
                                if let Err(error) = &commit_result {
                                    warn!(
                                        component = "db",
                                        operation = "batch_commit",
                                        result = "error",
                                        error = %error,
                                        "engine database worker failed to commit a batched transaction"
                                    );
                                }
                                for (reply, result) in outcomes {
                                    let result = match &commit_result {
                                        Ok(()) => result,
                                        Err(error) => Err(format!(
                                            "batched transaction failed to commit: {error}"
                                        )),
                                    };
                                    match reply {
                                        DbReply::Async(reply) => {
                                            let _ = reply.send(result);
                                        }
                                        #[cfg(not(test))]
                                        DbReply::Blocking(reply) => {
                                            let _ = reply.send(result);
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                let message = format!(
                                    "database worker could not open a transaction: {error}"
                                );
                                for (_, _, _, reply) in batch {
                                    match reply {
                                        DbReply::Async(reply) => {
                                            let _ = reply.send(Err(message.clone()));
                                        }
                                        #[cfg(not(test))]
                                        DbReply::Blocking(reply) => {
                                            let _ = reply.send(Err(message.clone()));
                                        }
                                    }
                                }
                            }
                        }
                        if worker_force_stop.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    DbRequest::Shutdown { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
            worker_healthy.store(false, Ordering::Release);
            warn!(
                component = "db",
                operation = "worker",
                result = "stopped",
                "engine database worker stopped"
            );
        });

        let thread = match thread {
            Ok(thread) => match ready_rx.recv_timeout(DB_STARTUP_TIMEOUT) {
                Ok(true) => Some(thread),
                Ok(false) => Some(thread),
                Err(error) => {
                    healthy.store(false, Ordering::Release);
                    force_stop.store(true, Ordering::Release);
                    warn!(
                        component = "db",
                        operation = "start_worker",
                        result = "timeout",
                        error = %error,
                        "engine database worker did not report startup before the deadline"
                    );
                    Some(thread)
                }
            },
            Err(error) => {
                healthy.store(false, Ordering::Release);
                warn!(
                    component = "db",
                    operation = "spawn_worker_thread",
                    result = "error",
                    error = %error,
                    "engine database worker thread could not start"
                );
                None
            }
        };
        Self {
            tx,
            healthy,
            force_stop,
            thread: Arc::new(Mutex::new(thread)),
        }
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Request an out-of-band stop for drop/abort cleanup. A normal shutdown
    /// still uses the queued sentinel so prior work drains; this fallback is
    /// used only when the owning actor has already been aborted or a bounded
    /// shutdown phase has timed out.
    #[cfg(not(test))]
    pub(crate) fn request_stop(&self) {
        self.healthy.store(false, Ordering::Release);
        self.force_stop();
    }

    fn force_stop(&self) {
        self.force_stop.store(true, Ordering::Release);
    }

    async fn enqueue(&self, mut request: DbRequest, operation: &'static str) -> Result<(), String> {
        let deadline = Instant::now() + DB_SEND_TIMEOUT;
        loop {
            match self.tx.try_send(request) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(returned)) => {
                    request = returned;
                    if Instant::now() >= deadline {
                        return Err(format!("database worker queue timed out for {operation}"));
                    }
                    sleep(DB_SEND_RETRY).await;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.healthy.store(false, Ordering::Release);
                    self.force_stop();
                    return Err("engine database worker stopped".to_owned());
                }
            }
        }
    }

    #[cfg(not(test))]
    fn enqueue_blocking(
        &self,
        mut request: DbRequest,
        operation: &'static str,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + DB_SEND_TIMEOUT;
        loop {
            match self.tx.try_send(request) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(returned)) => {
                    request = returned;
                    if std::time::Instant::now() >= deadline {
                        return Err(format!("database worker queue timed out for {operation}"));
                    }
                    std::thread::sleep(DB_SEND_RETRY);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.healthy.store(false, Ordering::Release);
                    self.force_stop();
                    return Err("engine database worker stopped".to_owned());
                }
            }
        }
    }

    /// Execute one operation in queue order and downcast its typed result.
    pub(crate) async fn run<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        self.run_with_reply_timeout(operation, job, DB_REPLY_TIMEOUT)
            .await
    }

    /// Like [`run`](Self::run), but the worker may run this job inside a
    /// transaction shared with other `run_batched` jobs already queued,
    /// instead of opening and committing a transaction per call. `job` must
    /// not open or commit its own transaction; it only ever sees the one
    /// the worker already opened for the batch it landed in — even a batch
    /// of one still runs through this same shared-transaction path.
    pub(crate) async fn run_batched<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, String> + Send + 'static,
    {
        if !self.is_healthy() {
            return Err("engine database worker is unavailable".to_owned());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancellation_guard = CancellationGuard(Arc::clone(&cancelled));
        let (reply, response) = oneshot::channel();
        let request = DbRequest::ExecuteBatched {
            operation,
            job: Box::new(move |tx| job(tx).map(|value| Box::new(value) as ErasedValue)),
            cancelled,
            reply: DbReply::Async(reply),
        };
        self.enqueue(request, operation).await?;
        let value = match timeout(DB_REPLY_TIMEOUT, response).await {
            Ok(response) => response.map_err(|_| {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                "engine database worker dropped its reply".to_owned()
            })??,
            Err(_) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                return Err(format!("database worker operation timed out: {operation}"));
            }
        };
        value
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| format!("database worker returned an invalid result for {operation}"))
    }

    async fn run_with_reply_timeout<T, F>(
        &self,
        operation: &'static str,
        job: F,
        reply_timeout: Duration,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        if !self.is_healthy() {
            return Err("engine database worker is unavailable".to_owned());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancellation_guard = CancellationGuard(Arc::clone(&cancelled));
        let (reply, response) = oneshot::channel();
        let request = DbRequest::Execute {
            operation,
            job: Box::new(move |db| job(db).map(|value| Box::new(value) as ErasedValue)),
            cancelled,
            reply: DbReply::Async(reply),
        };
        self.enqueue(request, operation).await?;
        let value = match timeout(reply_timeout, response).await {
            Ok(response) => response.map_err(|_| {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                "engine database worker dropped its reply".to_owned()
            })??,
            Err(_) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                return Err(format!("database worker operation timed out: {operation}"));
            }
        };
        value
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| format!("database worker returned an invalid result for {operation}"))
    }

    /// Synchronous adapter for already-detached blocking jobs that cannot
    /// await a Tokio response. The SQLite connection remains owned by this
    /// worker thread; only the detached caller waits for the result.
    #[cfg(not(test))]
    pub(crate) fn run_blocking<T, F>(&self, operation: &'static str, job: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        if !self.is_healthy() {
            return Err("engine database worker is unavailable".to_owned());
        }
        let (reply, response) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancellation_guard = CancellationGuard(Arc::clone(&cancelled));
        self.enqueue_blocking(
            DbRequest::Execute {
                operation,
                job: Box::new(move |db| job(db).map(|value| Box::new(value) as ErasedValue)),
                cancelled,
                reply: DbReply::Blocking(reply),
            },
            operation,
        )?;
        let value = match response.recv_timeout(DB_REPLY_TIMEOUT) {
            Ok(value) => value?,
            Err(error) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                return Err(format!(
                    "database worker operation {operation} failed: {error}"
                ));
            }
        };
        value
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| format!("database worker returned an invalid result for {operation}"))
    }

    /// Drain queued operations, then stop the worker within the supplied
    /// budget. A timeout is observable by health checks and logs; it does not
    /// block engine shutdown indefinitely.
    pub(crate) async fn shutdown(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        if !self.is_healthy() {
            self.force_stop();
            let Some(join_budget) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            self.join_thread(join_budget).await;
            return;
        }
        let (reply, response) = oneshot::channel();
        let Some(send_budget) = deadline.checked_duration_since(Instant::now()) else {
            self.healthy.store(false, Ordering::Release);
            self.force_stop();
            return;
        };
        match timeout(
            send_budget,
            self.enqueue(DbRequest::Shutdown { reply }, "shutdown"),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                warn!(
                    component = "db",
                    operation = "shutdown",
                    result = "send_error",
                    error = %error,
                    "database worker shutdown request could not be queued"
                );
                let Some(join_budget) = deadline.checked_duration_since(Instant::now()) else {
                    return;
                };
                self.join_thread(join_budget).await;
                return;
            }
            Err(_) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                warn!(
                    component = "db",
                    operation = "shutdown",
                    result = "send_timeout",
                    "database worker shutdown request timed out"
                );
                return;
            }
        }
        let Some(wait_budget) = deadline.checked_duration_since(Instant::now()) else {
            self.healthy.store(false, Ordering::Release);
            self.force_stop();
            return;
        };
        match timeout(wait_budget, response).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                warn!(
                    component = "db",
                    operation = "shutdown",
                    result = "ack_error",
                    "database worker dropped its shutdown acknowledgement"
                );
                let Some(join_budget) = deadline.checked_duration_since(Instant::now()) else {
                    return;
                };
                self.join_thread(join_budget).await;
                return;
            }
            Err(_) => {
                self.healthy.store(false, Ordering::Release);
                self.force_stop();
                warn!(
                    component = "db",
                    operation = "shutdown",
                    result = "drain_timeout",
                    "database worker did not drain before shutdown deadline"
                );
                return;
            }
        }
        self.healthy.store(false, Ordering::Release);
        let Some(join_budget) = deadline.checked_duration_since(Instant::now()) else {
            self.force_stop();
            return;
        };
        self.join_thread(join_budget).await;
    }

    async fn join_thread(&self, budget: Duration) {
        let thread = self.thread.lock().ok().and_then(|mut thread| thread.take());
        let Some(thread) = thread else {
            return;
        };
        if timeout(budget, tokio::task::spawn_blocking(move || thread.join()))
            .await
            .is_err()
        {
            self.healthy.store(false, Ordering::Release);
            self.force_stop();
            warn!(
                component = "db",
                operation = "join_worker_thread",
                result = "timeout",
                "database worker thread did not join before shutdown deadline"
            );
        }
    }
}

fn thread_builder() -> std::thread::Builder {
    std::thread::Builder::new().name("torrentng-db".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tempfile::NamedTempFile;

    fn worker() -> DbWorker {
        let file = NamedTempFile::new().expect("temporary database path");
        let path = file.path().to_path_buf();
        let worker = DbWorker::new(path);
        // Keep the test path alive for the worker's lifetime.
        std::mem::forget(file);
        worker
    }

    #[tokio::test]
    async fn startup_failure_is_reported_before_requests_are_accepted() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let worker = DbWorker::new(temp.path().join("missing").join("state.db"));

        assert!(!worker.is_healthy());
        assert_eq!(
            worker
                .run("after_startup_failure", |_| Ok::<_, String>(()))
                .await
                .expect_err("requests must fail after worker startup failure"),
            "engine database worker is unavailable"
        );
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn operations_are_serial_and_worker_survives_errors_and_panics() {
        let worker = worker();
        let order = Arc::new(AtomicUsize::new(0));
        let first_order = Arc::clone(&order);
        let first = worker
            .run("first", move |_| {
                assert_eq!(first_order.fetch_add(1, Ordering::SeqCst), 0);
                Ok::<_, String>(7_u32)
            })
            .await
            .expect("first operation");
        assert_eq!(first, 7);

        let error = worker
            .run::<(), _>("expected_failure", |_| Err("synthetic failure".to_owned()))
            .await
            .expect_err("failure should cross the worker boundary");
        assert_eq!(error, "synthetic failure");

        let panic = worker
            .run::<(), _>("expected_panic", |_| panic!("synthetic panic"))
            .await
            .expect_err("panic should be contained by the worker");
        assert!(panic.contains("panicked"));

        let second_order = Arc::clone(&order);
        let second = worker
            .run("second", move |_| {
                assert_eq!(second_order.fetch_add(1, Ordering::SeqCst), 1);
                Ok::<_, String>(11_u32)
            })
            .await
            .expect("second operation");
        assert_eq!(second, 11);
        assert!(worker.is_healthy());
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn worker_owns_a_real_sqlite_connection_and_persists_ordered_work() {
        let file = NamedTempFile::new().expect("temporary database path");
        let path = file.path().to_path_buf();
        let worker = DbWorker::new(path.clone());
        std::mem::forget(file);

        worker
            .run("create_test_table", |db| {
                db.execute_batch(
                    "CREATE TABLE worker_probe (id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
                )
                .map_err(|error| error.to_string())
            })
            .await
            .expect("create test table");
        worker
            .run("insert_test_row", |db| {
                db.execute(
                    "INSERT INTO worker_probe (value) VALUES (?1)",
                    ["owned-by-worker"],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .await
            .expect("insert test row");
        let count: i64 = worker
            .run("read_test_row", |db| {
                db.query_row("SELECT COUNT(*) FROM worker_probe", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .await
            .expect("read test row");
        assert_eq!(count, 1);
        worker.shutdown(Duration::from_secs(1)).await;

        let db = Connection::open(path).expect("reopen worker database");
        let value: String = db
            .query_row("SELECT value FROM worker_probe WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("read persisted worker row");
        assert_eq!(value, "owned-by-worker");
    }

    #[tokio::test]
    async fn worker_enables_foreign_key_enforcement() {
        let worker = worker();
        let foreign_keys: i64 = worker
            .run("foreign_keys", |db| {
                db.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                    .map_err(|error| error.to_string())
            })
            .await
            .expect("read worker foreign-key setting");

        assert_eq!(foreign_keys, 1);
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn run_batched_persists_a_single_job_like_run_does() {
        let worker = worker();
        worker
            .run("create_probe", |db| {
                db.execute_batch(
                    "CREATE TABLE batch_probe (id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
                )
                .map_err(|error| error.to_string())
            })
            .await
            .expect("create probe table");

        worker
            .run_batched("insert_via_batch", |tx| {
                tx.execute(
                    "INSERT INTO batch_probe (value) VALUES (?1)",
                    ["solo-batched-job"],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .await
            .expect("batched insert");

        let value: String = worker
            .run("read_probe", |db| {
                db.query_row("SELECT value FROM batch_probe WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .map_err(|error| error.to_string())
            })
            .await
            .expect("read batched row");
        assert_eq!(value, "solo-batched-job");
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn run_batched_panic_is_contained_and_worker_survives() {
        let worker = worker();
        let panic = worker
            .run_batched::<(), _>("expected_batched_panic", |_tx| {
                panic!("synthetic batched panic")
            })
            .await
            .expect_err("panic should be contained by the worker");
        assert!(panic.contains("panicked"));

        // The worker must still be usable after a panic inside a batch.
        let value = worker
            .run_batched("after_batched_panic", |_tx| Ok::<_, String>(9_u32))
            .await
            .expect("worker keeps serving batched jobs after a panic");
        assert_eq!(value, 9);
        assert!(worker.is_healthy());
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn one_erroring_batched_job_does_not_lose_concurrent_siblings() {
        let worker = worker();
        worker
            .run("create_probe", |db| {
                db.execute_batch(
                    "CREATE TABLE concurrent_batch_probe (id INTEGER PRIMARY KEY, tag INTEGER NOT NULL);",
                )
                .map_err(|error| error.to_string())
            })
            .await
            .expect("create probe table");

        // Fire several batched writes and one deliberately-erroring batched
        // job concurrently, without awaiting between sends, so the worker
        // has a real chance to coalesce them into one shared transaction.
        // Every successful job must still see its own row persisted, and
        // the erroring job must not silently swallow or corrupt siblings'
        // results even if they land in the same transaction.
        let worker_ref = &worker;
        let error_job = worker_ref.run_batched::<(), _>("erroring_sibling", |_tx| {
            Err("synthetic sibling failure".to_owned())
        });
        let writes = (0..8i64).map(|tag| {
            worker_ref.run_batched("ok_sibling", move |tx| {
                tx.execute(
                    "INSERT INTO concurrent_batch_probe (tag) VALUES (?1)",
                    [tag],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
        });
        let (error_result, write_results) =
            tokio::join!(error_job, futures::future::join_all(writes));

        assert_eq!(
            error_result.expect_err("erroring job must report its own error"),
            "synthetic sibling failure"
        );
        for result in write_results {
            result.expect("sibling write must succeed independently of the erroring job");
        }

        let count: i64 = worker
            .run("count_probe", |db| {
                db.query_row("SELECT COUNT(*) FROM concurrent_batch_probe", [], |row| {
                    row.get(0)
                })
                .map_err(|error| error.to_string())
            })
            .await
            .expect("count persisted rows");
        assert_eq!(count, 8);
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn mixed_execute_and_execute_batched_requests_all_complete_correctly() {
        let worker = worker();
        let worker_ref = &worker;
        // A plain `Execute` job queued alongside several `ExecuteBatched`
        // jobs must be handled correctly by the drain loop's
        // not-part-of-this-batch hold-for-next-turn path, without being
        // lost, duplicated, or corrupting the batch it interrupted.
        let plain = worker_ref.run("plain_job", |_db| Ok::<_, String>("plain".to_owned()));
        let batched = (0..5u32).map(|n| worker_ref.run_batched("batched_job", move |_tx| Ok::<_, String>(n)));
        let (plain_result, batched_results) = tokio::join!(plain, futures::future::join_all(batched));

        assert_eq!(plain_result.expect("plain job"), "plain");
        let mut values: Vec<u32> = batched_results
            .into_iter()
            .map(|result| result.expect("batched job"))
            .collect();
        values.sort_unstable();
        assert_eq!(values, vec![0, 1, 2, 3, 4]);
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn sqlite_failure_crosses_boundary_and_worker_continues() {
        let worker = worker();
        worker
            .run("create_failure_probe", |db| {
                db.execute_batch(
                    "CREATE TABLE worker_failure_probe (value TEXT NOT NULL);
                     CREATE TRIGGER worker_failure_trigger
                     BEFORE INSERT ON worker_failure_probe
                     BEGIN SELECT RAISE(ABORT, 'injected sqlite failure'); END;",
                )
                .map_err(|error| error.to_string())
            })
            .await
            .expect("create failure probe");
        let failure = worker
            .run::<(), _>("injected_sqlite_failure", |db| {
                db.execute(
                    "INSERT INTO worker_failure_probe (value) VALUES ('rejected')",
                    [],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .await
            .expect_err("SQLite trigger failure should cross worker boundary");
        assert!(failure.contains("injected sqlite failure"), "{failure}");
        worker
            .run("remove_failure_probe", |db| {
                db.execute_batch("DROP TRIGGER worker_failure_trigger;")
                    .map_err(|error| error.to_string())
            })
            .await
            .expect("remove failure trigger");
        worker
            .run("write_after_failure", |db| {
                db.execute(
                    "INSERT INTO worker_failure_probe (value) VALUES ('accepted')",
                    [],
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
            })
            .await
            .expect("worker should accept work after SQLite failure");
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn cancelled_queued_operation_is_not_applied() {
        let worker = worker();
        let (first_started_tx, first_started) = oneshot::channel();
        let release_first = Arc::new(std::sync::Barrier::new(2));
        let release_for_first = Arc::clone(&release_first);
        let first = tokio::spawn({
            let worker = worker.clone();
            async move {
                worker
                    .run("blocking_first", move |_| {
                        let _ = first_started_tx.send(());
                        release_for_first.wait();
                        Ok::<_, String>(())
                    })
                    .await
            }
        });
        timeout(Duration::from_secs(1), first_started)
            .await
            .expect("first operation started before timeout")
            .expect("first started signal");

        let applied = Arc::new(AtomicUsize::new(0));
        let applied_for_cancelled = Arc::clone(&applied);
        let cancelled = tokio::spawn({
            let worker = worker.clone();
            async move {
                worker
                    .run("cancelled_write", move |_| {
                        applied_for_cancelled.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, String>(())
                    })
                    .await
            }
        });
        tokio::task::yield_now().await;
        cancelled.abort();
        cancelled
            .await
            .expect_err("cancelled operation task should be aborted");
        release_first.wait();
        first
            .await
            .expect("first operation task")
            .expect("first operation");

        worker
            .run("after_cancel", |_| Ok::<_, String>(()))
            .await
            .expect("worker should remain usable after cancellation");
        assert_eq!(applied.load(Ordering::SeqCst), 0);
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn shutdown_drains_prior_work() {
        let worker = worker();
        let seen = Arc::new(AtomicUsize::new(0));
        let seen_for_job = Arc::clone(&seen);
        let worker_for_pending = worker.clone();
        let pending = tokio::spawn(async move {
            worker_for_pending
                .run("pending", move |_| {
                    seen_for_job.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(())
                })
                .await
        });
        while seen.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        worker.shutdown(Duration::from_secs(1)).await;
        pending
            .await
            .expect("pending task")
            .expect("pending operation");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert!(!worker.is_healthy());
    }

    #[tokio::test]
    async fn shutdown_timeout_marks_worker_unhealthy() {
        let worker = worker();
        let (started_tx, started) = oneshot::channel();
        let release = Arc::new(std::sync::Barrier::new(2));
        let release_for_job = Arc::clone(&release);
        let pending = tokio::spawn({
            let worker = worker.clone();
            async move {
                worker
                    .run("blocking_shutdown", move |_| {
                        let _ = started_tx.send(());
                        release_for_job.wait();
                        Ok::<_, String>(())
                    })
                    .await
            }
        });
        timeout(Duration::from_secs(1), started)
            .await
            .expect("blocking operation started before shutdown")
            .expect("started signal");

        worker.shutdown(Duration::from_millis(25)).await;
        assert!(!worker.is_healthy());

        release.wait();
        pending
            .await
            .expect("blocking operation task")
            .expect("blocking operation");
        worker.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn zero_budget_shutdown_forces_stop_before_enqueue() {
        let worker = worker();

        worker.shutdown(Duration::ZERO).await;

        assert!(!worker.is_healthy());
        assert!(worker.force_stop.load(Ordering::Acquire));
        assert_eq!(
            worker
                .run("after_zero_budget_shutdown", |_| Ok::<_, String>(()))
                .await
                .expect_err("stopped worker must reject new operations"),
            "engine database worker is unavailable"
        );
    }

    #[tokio::test]
    async fn shutdown_queue_timeout_forces_worker_to_stop() {
        let worker = worker();
        let (started_tx, started) = oneshot::channel();
        let release = Arc::new(std::sync::Barrier::new(2));
        let release_for_job = Arc::clone(&release);
        let pending = tokio::spawn({
            let worker = worker.clone();
            async move {
                worker
                    .run("blocking_full_queue", move |_| {
                        let _ = started_tx.send(());
                        release_for_job.wait();
                        Ok::<_, String>(())
                    })
                    .await
            }
        });
        timeout(Duration::from_secs(1), started)
            .await
            .expect("blocking operation started before queue fill")
            .expect("started signal");

        let executed = Arc::new(AtomicUsize::new(0));
        let mut queued_replies = Vec::with_capacity(DB_QUEUE_CAPACITY);
        for _ in 0..DB_QUEUE_CAPACITY {
            let (reply, response) = oneshot::channel();
            let executed_for_job = Arc::clone(&executed);
            worker
                .tx
                .try_send(DbRequest::Execute {
                    operation: "queued_after_shutdown_timeout",
                    job: Box::new(move |_| {
                        executed_for_job.fetch_add(1, Ordering::SeqCst);
                        Ok::<ErasedValue, String>(Box::new(()))
                    }),
                    cancelled: Arc::new(AtomicBool::new(false)),
                    reply: DbReply::Async(reply),
                })
                .expect("worker queue should accept the test workload");
            queued_replies.push(response);
        }

        worker.shutdown(Duration::from_millis(25)).await;
        assert!(!worker.is_healthy());

        release.wait();
        pending
            .await
            .expect("blocking operation task")
            .expect("blocking operation");
        worker.shutdown(Duration::from_secs(1)).await;
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        drop(queued_replies);
    }

    #[tokio::test]
    async fn operation_timeout_marks_worker_unhealthy() {
        let worker = worker();
        let (started_tx, started) = oneshot::channel();
        let release = Arc::new(std::sync::Barrier::new(2));
        let release_for_job = Arc::clone(&release);
        let pending = tokio::spawn({
            let worker = worker.clone();
            async move {
                worker
                    .run_with_reply_timeout(
                        "blocking_operation_timeout",
                        move |_| {
                            let _ = started_tx.send(());
                            release_for_job.wait();
                            Ok::<_, String>(())
                        },
                        Duration::from_millis(100),
                    )
                    .await
            }
        });
        timeout(Duration::from_secs(1), started)
            .await
            .expect("blocking operation started before timeout")
            .expect("started signal");

        let result = pending.await.expect("timed operation task");
        assert_eq!(
            result.expect_err("operation should time out"),
            "database worker operation timed out: blocking_operation_timeout"
        );
        assert!(!worker.is_healthy());

        release.wait();
        worker.shutdown(Duration::from_secs(1)).await;
    }
}
