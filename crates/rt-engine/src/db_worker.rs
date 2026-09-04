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

type ErasedValue = Box<dyn Any + Send>;
type DbOperation = Box<dyn FnOnce(&mut Connection) -> Result<ErasedValue, String> + Send>;
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
                    return;
                }
            };
            if let Err(error) = db.busy_timeout(Duration::from_secs(5)) {
                worker_healthy.store(false, Ordering::Release);
                warn!(
                    component = "db",
                    operation = "configure_worker_connection",
                    result = "error",
                    error = %error,
                    "engine database worker could not configure its connection"
                );
                return;
            }

            while let Ok(request) = rx.recv() {
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
            Ok(thread) => Some(thread),
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
            thread: Arc::new(Mutex::new(thread)),
        }
    }

    pub(crate) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
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
        let value = timeout(DB_REPLY_TIMEOUT, response)
            .await
            .map_err(|_| format!("database worker operation timed out: {operation}"))?
            .map_err(|_| "engine database worker dropped its reply".to_owned())??;
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
        self.enqueue_blocking(
            DbRequest::Execute {
                operation,
                job: Box::new(move |db| job(db).map(|value| Box::new(value) as ErasedValue)),
                cancelled: Arc::new(AtomicBool::new(false)),
                reply: DbReply::Blocking(reply),
            },
            operation,
        )?;
        let value = response
            .recv_timeout(DB_REPLY_TIMEOUT)
            .map_err(|error| format!("database worker operation {operation} failed: {error}"))??;
        value
            .downcast::<T>()
            .map(|value| *value)
            .map_err(|_| format!("database worker returned an invalid result for {operation}"))
    }

    /// Drain queued operations, then stop the worker within the supplied
    /// budget. A timeout is observable by health checks and logs; it does not
    /// block engine shutdown indefinitely.
    pub(crate) async fn shutdown(&self, budget: Duration) {
        if !self.is_healthy() {
            self.join_thread(budget).await;
            return;
        }
        let (reply, response) = oneshot::channel();
        if timeout(
            budget,
            self.enqueue(DbRequest::Shutdown { reply }, "shutdown"),
        )
        .await
        .is_err()
        {
            warn!(
                component = "db",
                operation = "shutdown",
                result = "send_timeout",
                "database worker shutdown request timed out"
            );
            return;
        }
        if timeout(budget, response).await.is_err() {
            warn!(
                component = "db",
                operation = "shutdown",
                result = "drain_timeout",
                "database worker did not drain before shutdown deadline"
            );
            return;
        }
        self.healthy.store(false, Ordering::Release);
        self.join_thread(budget).await;
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
}
