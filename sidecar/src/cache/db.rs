use super::categories::{
    check_category_capacity, check_tag_capacity, check_torrent_tag_capacity,
    MAX_CACHED_LABEL_NAME_BYTES, MAX_CACHED_TAGS_PER_MUTATION, MAX_CACHED_TAG_MUTATION_BYTES,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, OnceLock, TryLockError,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

/// qBittorrent's `sync/maindata.rid` is a logical change cursor, not a wall
/// clock.  Keep it in the cache database so it survives service restarts and
/// cannot miss two updates made in the same second.
pub(crate) const CACHE_REVISION_KEY: &str = "cache_revision";
pub(crate) const CACHE_REVISION_FLOOR_KEY: &str = "cache_revision_floor";
pub(crate) const MAX_REMOVED_TORRENT_TOMBSTONES: i64 = 100_000;
const MAX_BLOCKING_DB_READS: usize = 8;
/// Number of standing read-only connections kept open beside the single
/// writer connection. SQLite's WAL mode lets any number of reader
/// connections run concurrently with the one writer connection without
/// blocking each other, as long as each side has its own `Connection`
/// handle; this pool exists purely so reads stop serializing behind the
/// writer's mutex. Kept small and fixed-size -- this is a read cache in
/// front of a poll loop, not a general-purpose connection pool.
const READ_POOL_SIZE: usize = 4;

fn parse_cached_tags(raw: &str) -> Result<Vec<String>> {
    if raw.len() > MAX_CACHED_TAG_MUTATION_BYTES {
        anyhow::bail!("cached torrent tag list exceeds the byte limit");
    }
    let mut tags = Vec::new();
    for tag in raw.split(',').map(str::trim).filter(|tag| !tag.is_empty()) {
        if tags.len() >= MAX_CACHED_TAGS_PER_MUTATION {
            anyhow::bail!(
                "cached torrent tag list exceeds the {MAX_CACHED_TAGS_PER_MUTATION}-item limit"
            );
        }
        if tag.len() > MAX_CACHED_LABEL_NAME_BYTES {
            anyhow::bail!("cached tag name exceeds the {MAX_CACHED_LABEL_NAME_BYTES}-byte limit");
        }
        tags.push(tag.to_owned());
    }
    Ok(tags)
}
static BLOCKING_DB_READ_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Debug, Clone, serde::Serialize)]
pub struct TorrentRow {
    pub hash: String,
    pub name: String,
    pub size_bytes: i64,
    pub bytes_done: i64,
    pub down_rate: i64,
    pub up_rate: i64,
    pub up_total: i64,
    pub down_total: i64,
    pub ratio: i64,
    pub is_active: bool,
    pub is_open: bool,
    pub complete: bool,
    pub state: i64,
    pub priority: i64,
    pub category: String,
    pub base_path: String,
    pub directory: String,
    pub creation_date: i64,
    pub timestamp_finished: i64,
    pub tracker_focus: i64,
    pub peers_connected: i64,
    pub peers_complete: i64,
    pub message: String,
    pub tracker_url: String,
    pub tags: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AppEventRow {
    pub event_id: Option<i64>,
    pub occurred_at: i64,
    pub level: String,
    pub kind: String,
    pub message: String,
    pub payload: String,
}

/// Shared state behind `Db`: one dedicated writer connection (mutex-guarded,
/// matching SQLite/WAL's single-writer model) and a small pool of read-only
/// connections that can run concurrently with the writer and with each
/// other.
struct Inner {
    writer: Mutex<Connection>,
    writer_poison_reported: AtomicBool,
    readers: ReadPool,
}

/// A small fixed-size, round-robin pool of read-only SQLite connections.
///
/// Checkout tries each connection starting from a rotating index and takes
/// the first one that isn't currently busy, so concurrent readers spread
/// across the pool instead of queuing on a single slot; if every connection
/// is busy, the caller blocks on its assigned slot rather than spinning.
struct ReadPool {
    conns: Vec<Mutex<Connection>>,
    next: AtomicUsize,
    poison_reported: AtomicBool,
}

impl ReadPool {
    fn open(path: &Path, size: usize) -> Result<Self> {
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = Connection::open(path)
                .with_context(|| format!("open sqlite read connection {}", path.display()))?;
            apply_pragmas(&conn)?;
            // Reader connections exist only to run SELECTs concurrently
            // with the writer; refuse any accidental write outright rather
            // than letting one silently succeed outside the writer's
            // serialization point.
            conn.execute_batch("PRAGMA query_only=ON;")?;
            register_media_type_function(&conn)?;
            conns.push(Mutex::new(conn));
        }
        Ok(Self {
            conns,
            next: AtomicUsize::new(0),
            poison_reported: AtomicBool::new(false),
        })
    }

    fn checkout(&self) -> Result<MutexGuard<'_, Connection>> {
        let len = self.conns.len();
        if len == 0 {
            anyhow::bail!("cache read connection pool is empty");
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed) % len;
        for offset in 0..len {
            let idx = (start + offset) % len;
            match self.conns[idx].try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Poisoned(_)) => self.note_poisoned_connection(),
            }
        }
        // Every healthy connection is currently busy: block on the first
        // usable slot rather than spinning. A poisoned slot is skipped so a
        // single failed reader cannot turn later requests into repeat panics.
        for offset in 0..len {
            let idx = (start + offset) % len;
            match self.conns[idx].lock() {
                Ok(guard) => return Ok(guard),
                Err(_) => self.note_poisoned_connection(),
            }
        }
        anyhow::bail!("cache read connection pool has no healthy connections")
    }

    fn note_poisoned_connection(&self) {
        if !self.poison_reported.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                component = "db",
                operation = "read_connection",
                result = "mutex_poisoned",
                "skipping poisoned cache read connection"
            );
        }
    }
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct Db(Arc<Inner>);

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cache dir {}", parent.display()))?;
        }

        let mut writer_conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;

        apply_pragmas(&writer_conn)?;

        register_media_type_function(&writer_conn)?;
        migrate(&mut writer_conn)?;

        // Open the read pool only after migration has run so every reader
        // sees the final schema; opening against the same path picks up the
        // WAL mode the writer just enabled.
        let readers = ReadPool::open(path, READ_POOL_SIZE)
            .with_context(|| format!("open sqlite read pool {}", path.display()))?;

        Ok(Self(Arc::new(Inner {
            writer: Mutex::new(writer_conn),
            writer_poison_reported: AtomicBool::new(false),
            readers,
        })))
    }

    /// Acquire the dedicated writer connection. Every mutation must go
    /// through this so writes stay serialized -- WAL allows exactly one
    /// writer connection at a time.
    pub(crate) fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        match self.0.writer.lock() {
            Ok(guard) => Ok(guard),
            Err(_) => {
                if !self.0.writer_poison_reported.swap(true, Ordering::AcqRel) {
                    tracing::error!(
                        component = "db",
                        operation = "writer_connection",
                        result = "mutex_poisoned",
                        "cache writer connection is unavailable after mutex poisoning"
                    );
                }
                anyhow::bail!("cache writer connection is unavailable after mutex poisoning")
            }
        }
    }

    /// Check out a connection from the read-only pool. Safe to call
    /// concurrently with other reads and with an in-progress write: WAL lets
    /// readers proceed against the last-committed snapshot without blocking
    /// on, or being blocked by, the writer.
    pub(crate) fn read(&self) -> Result<MutexGuard<'_, Connection>> {
        self.0.readers.checkout()
    }

    /// Execute a cache operation on Tokio's blocking pool. The cache is
    /// synchronous rusqlite underneath (one writer connection plus a small
    /// read-only connection pool); calling it directly from an async HTTP
    /// handler lets a slow SQLite call occupy an executor worker and
    /// amplifies latency for unrelated requests.
    pub async fn run_blocking<T, F>(&self, operation: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Db) -> Result<T> + Send + 'static,
    {
        let gate = BLOCKING_DB_READ_GATE
            .get_or_init(|| Arc::new(Semaphore::new(MAX_BLOCKING_DB_READS)))
            .clone();
        let permit = gate
            .acquire_owned()
            .await
            .with_context(|| format!("cache blocking task admission failed: {operation}"))?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f(&db)
        })
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "cache blocking task failed: {}",
                crate::task_join_error_summary(operation, &error)
            )
        })?
    }

    pub fn upsert(&self, t: &TorrentRow) -> Result<()> {
        self.upsert_with_tags(t, false).map(|_| ())
    }

    /// Upsert a backend projection and optionally reconcile its authoritative
    /// tag set. Backends without tag support must pass `false`: an empty
    /// `TorrentRow::tags` in that case means "not exposed", not "remove every
    /// cached tag".
    pub fn upsert_with_tags(&self, t: &TorrentRow, sync_tags: bool) -> Result<bool> {
        if t.category.len() > MAX_CACHED_LABEL_NAME_BYTES {
            anyhow::bail!("category name exceeds the {MAX_CACHED_LABEL_NAME_BYTES}-byte limit");
        }
        let mut desired_tags = if sync_tags {
            parse_cached_tags(&t.tags)?
        } else {
            Vec::new()
        };
        desired_tags.sort_unstable();
        desired_tags.dedup();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        // qBittorrent treats info hashes case-insensitively.  The legacy
        // schema has a binary-collated primary key, so resolve an existing
        // row before the INSERT ... ON CONFLICT path or a differently-cased
        // refresh would create a second logical torrent.
        let hash = canonical_hash(&tx, &t.hash)?.unwrap_or_else(|| t.hash.clone());
        check_category_capacity(&tx, Some(&hash), &t.category)?;
        if sync_tags {
            let tag_refs = desired_tags.iter().map(String::as_str).collect::<Vec<_>>();
            check_tag_capacity(&tx, &tag_refs)?;
            check_torrent_tag_capacity(&tx, &hash, &tag_refs, false)?;
        }
        let tags_changed = if sync_tags {
            let (current_count, max_tag_bytes): (i64, i64) = tx.query_row(
                "SELECT COUNT(*), COALESCE(MAX(length(CAST(tag AS BLOB))), 0)
                 FROM torrent_tags WHERE hash=?1 COLLATE NOCASE",
                params![hash.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if current_count < 0
                || current_count as usize > MAX_CACHED_TAGS_PER_MUTATION
                || max_tag_bytes < 0
                || max_tag_bytes as usize > MAX_CACHED_LABEL_NAME_BYTES
            {
                // A backend refresh is allowed to repair an over-limit legacy
                // projection; avoid collecting it into an unbounded Vec first.
                true
            } else {
                let mut stmt = tx.prepare(
                    "SELECT tag FROM torrent_tags WHERE hash=?1 COLLATE NOCASE
                     ORDER BY tag LIMIT ?2",
                )?;
                let rows = stmt
                    .query_map(
                        params![hash.as_str(), (MAX_CACHED_TAGS_PER_MUTATION + 1) as i64],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows != desired_tags
            }
        } else {
            false
        };

        // `updated_at` is a last-seen timestamp, not a logical content
        // change. Sync runs may refresh it without waking every qBittorrent
        // `sync/maindata` client with the entire library. Allocate a revision
        // only when a projected field or authoritative tag set changed.
        let projection_unchanged: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM torrents
                WHERE hash=?1
                  AND name IS ?2
                  AND size_bytes IS ?3
                  AND bytes_done IS ?4
                  AND down_rate IS ?5
                  AND up_rate IS ?6
                  AND up_total IS ?7
                  AND down_total IS ?8
                  AND ratio IS ?9
                  AND is_active IS ?10
                  AND is_open IS ?11
                  AND complete IS ?12
                  AND state IS ?13
                  AND priority IS ?14
                  AND category IS ?15
                  AND base_path IS ?16
                  AND directory IS ?17
                  AND creation_date IS ?18
                  AND timestamp_finished IS ?19
                  AND tracker_focus IS ?20
                  AND peers_connected IS ?21
                  AND peers_complete IS ?22
                  AND message IS ?23
                  AND tracker_url IS ?24
            )",
            params![
                hash.as_str(),
                t.name,
                t.size_bytes,
                t.bytes_done,
                t.down_rate,
                t.up_rate,
                t.up_total,
                t.down_total,
                t.ratio,
                t.is_active as i64,
                t.is_open as i64,
                t.complete as i64,
                t.state,
                t.priority,
                t.category,
                t.base_path,
                t.directory,
                t.creation_date,
                t.timestamp_finished,
                t.tracker_focus,
                t.peers_connected,
                t.peers_complete,
                t.message,
                t.tracker_url,
            ],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if projection_unchanged && !tags_changed {
            tx.execute(
                "UPDATE torrents SET updated_at=?1 WHERE hash=?2 COLLATE NOCASE",
                params![t.updated_at, hash],
            )?;
            tx.commit()?;
            return Ok(false);
        }
        let revision = allocate_revision(&tx)?;
        tx.execute(
            // tags is excluded from DO UPDATE — managed via torrent_tags table
            "INSERT INTO torrents (
                hash, name, size_bytes, bytes_done, down_rate, up_rate,
                up_total, down_total, ratio, is_active, is_open, complete,
                state, priority, category, base_path, directory, creation_date,
                timestamp_finished, tracker_focus, peers_connected, peers_complete,
                message, tracker_url, tags, updated_at, revision
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                      ?17,?18,?19,?20,?21,?22,?23,?24,'',?25,?26)
            ON CONFLICT(hash) DO UPDATE SET
                name=excluded.name, size_bytes=excluded.size_bytes,
                bytes_done=excluded.bytes_done, down_rate=excluded.down_rate,
                up_rate=excluded.up_rate, up_total=excluded.up_total,
                down_total=excluded.down_total, ratio=excluded.ratio,
                is_active=excluded.is_active, is_open=excluded.is_open,
                complete=excluded.complete, state=excluded.state,
                priority=excluded.priority, category=excluded.category,
                base_path=excluded.base_path, directory=excluded.directory,
                creation_date=excluded.creation_date,
                timestamp_finished=excluded.timestamp_finished,
                tracker_focus=excluded.tracker_focus,
                peers_connected=excluded.peers_connected,
                peers_complete=excluded.peers_complete,
                message=excluded.message,
                tracker_url=excluded.tracker_url,
                updated_at=excluded.updated_at,
                revision=excluded.revision",
            params![
                hash,
                t.name,
                t.size_bytes,
                t.bytes_done,
                t.down_rate,
                t.up_rate,
                t.up_total,
                t.down_total,
                t.ratio,
                t.is_active as i64,
                t.is_open as i64,
                t.complete as i64,
                t.state,
                t.priority,
                t.category,
                t.base_path,
                t.directory,
                t.creation_date,
                t.timestamp_finished,
                t.tracker_focus,
                t.peers_connected,
                t.peers_complete,
                t.message,
                t.tracker_url,
                t.updated_at,
                revision,
            ],
        )
        .context("upsert torrent")?;
        if sync_tags {
            tx.execute(
                "DELETE FROM torrent_tags WHERE hash=?1 COLLATE NOCASE",
                params![hash.as_str()],
            )?;
            for tag in &desired_tags {
                tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
                tx.execute(
                    "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
                    params![hash.as_str(), tag],
                )?;
            }
        }
        // Re-adding a previously deleted torrent makes its current row the
        // authoritative state. The old tombstone must not be emitted beside
        // the new row for future cursors.
        tx.execute(
            "DELETE FROM removed_torrents WHERE hash=?1 COLLATE NOCASE",
            params![hash],
        )?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn delete(&self, hash: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let Some(canonical) = canonical_hash(&tx, hash)? else {
            tx.commit()?;
            return Ok(());
        };

        let revision = allocate_revision(&tx)?;
        tx.execute("DELETE FROM torrents WHERE hash=?1", params![canonical])?;
        // Clean up any tombstone written with a different case by an older
        // compatible-client service before inserting the canonical key.
        tx.execute(
            "DELETE FROM removed_torrents WHERE hash=?1 COLLATE NOCASE",
            params![canonical],
        )?;
        tx.execute(
            "INSERT INTO removed_torrents(hash, revision) VALUES(?1, ?2)
             ON CONFLICT(hash) DO UPDATE SET revision=excluded.revision",
            params![canonical, revision],
        )?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    /// Return the durable qBittorrent-compatible cache revision. This is
    /// intentionally independent from `TorrentRow::updated_at`, which is a
    /// wall-clock freshness value and is not a safe change cursor.
    pub fn current_revision(&self) -> Result<i64> {
        let conn = self.read()?;
        current_revision_locked(&conn)
    }

    pub fn append_app_event(&self, event: &AppEventRow, retention: usize) -> Result<i64> {
        serde_json::from_str::<serde_json::Value>(&event.payload)
            .with_context(|| "validate app event payload JSON")?;
        let message = crate::url_redaction::redact_sensitive_text(&event.message);
        let payload = crate::url_redaction::redact_sensitive_json(&event.payload);
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO app_events (occurred_at, level, kind, message, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event.occurred_at, event.level, event.kind, message, payload,],
        )?;
        let id = conn.last_insert_rowid();
        prune_app_events_locked(&conn, retention.max(1))?;
        Ok(id)
    }

    pub fn list_app_events(&self, limit: usize) -> Result<Vec<AppEventRow>> {
        self.list_app_events_filtered(limit, None, &[], None)
    }

    pub fn list_app_events_filtered(
        &self,
        limit: usize,
        kind: Option<&str>,
        levels: &[&str],
        last_known_id: Option<i64>,
    ) -> Result<Vec<AppEventRow>> {
        let conn = self.read()?;
        let limit = limit.max(1) as i64;
        let mut sql = "SELECT event_id, occurred_at, level, kind, message, payload
             FROM app_events"
            .to_owned();
        let mut clauses = Vec::new();
        if kind.is_some() {
            clauses.push("kind = ?");
        }
        if last_known_id.is_some() {
            clauses.push("event_id > ?");
        }
        let level_placeholders = if levels.is_empty() {
            String::new()
        } else {
            std::iter::repeat_n("?", levels.len())
                .collect::<Vec<_>>()
                .join(",")
        };
        let level_clause;
        if !levels.is_empty() {
            level_clause = format!("lower(level) IN ({level_placeholders})");
            clauses.push(&level_clause);
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY event_id DESC LIMIT ?");

        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(kind) = kind {
            values.push(rusqlite::types::Value::Text(kind.to_owned()));
        }
        if let Some(last_known_id) = last_known_id {
            values.push(rusqlite::types::Value::Integer(last_known_id));
        }
        for level in levels {
            values.push(rusqlite::types::Value::Text(level.to_ascii_lowercase()));
        }
        values.push(rusqlite::types::Value::Integer(limit));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                let message: String = row.get(4)?;
                let payload: String = row.get(5)?;
                Ok(AppEventRow {
                    event_id: Some(row.get(0)?),
                    occurred_at: row.get(1)?,
                    level: row.get(2)?,
                    kind: row.get(3)?,
                    message: crate::url_redaction::redact_sensitive_text(&message),
                    payload: crate::url_redaction::redact_sensitive_json(&payload),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .read()?
            .query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_kv(&self, key: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM kv WHERE key=?1", params![key])?;
        Ok(())
    }

    pub fn exists(&self, hash: &str) -> Result<bool> {
        let exists: i64 = self.read()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE hash=?1 COLLATE NOCASE)",
            params![hash],
            |r| r.get(0),
        )?;
        Ok(exists != 0)
    }

    /// Return the cache's canonical spelling for a logical torrent hash.
    ///
    /// Hashes are case-insensitive at the protocol boundary, but the cache
    /// keeps the original spelling as the primary-key value. Compatibility
    /// mutations must resolve that value before handing a target to a
    /// backend; otherwise a backend that treats an unknown id as a no-op can
    /// make a successful HTTP response lie about the requested torrent.
    pub fn canonical_hash(&self, hash: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        canonical_hash(&conn, hash)
    }

    pub fn count(&self) -> Result<i64> {
        let n: i64 = self
            .read()?
            .query_row("SELECT COUNT(*) FROM torrents", [], |r| r.get(0))?;
        Ok(n)
    }

    /// Return the cached torrent counters without materializing every row.
    ///
    /// Bounded backend synchronization updates the cache one page at a time;
    /// counting only the current page would publish page-local metrics as if
    /// they were library totals. Keep the aggregation in SQLite and run it at
    /// the end of a complete sync cycle.
    pub fn sync_counts(&self) -> Result<(i64, i64, i64, i64, i64)> {
        Ok(self.read()?.query_row(
            "SELECT
                COALESCE(SUM(CASE
                    WHEN state = 3 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE
                    WHEN state != 3 AND is_active = 0
                    THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE
                    WHEN state != 3 AND is_active != 0
                         AND complete != 0
                    THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE
                    WHEN state != 3 AND is_active != 0
                         AND complete = 0
                    THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(peers_connected), 0)
             FROM torrents",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?)
    }

    pub fn all_hashes(&self) -> Result<HashSet<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare("SELECT hash FROM torrents")?;
        let hashes = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(hashes)
    }

    /// Read no more than `limit` torrent hashes. Callers that need to reject
    /// over-limit selections can request `max + 1` rows without materializing
    /// the entire cache first.
    pub fn all_hashes_bounded(&self, limit: usize) -> Result<Vec<String>> {
        let limit = i64::try_from(limit).context("hash limit does not fit SQLite integer")?;
        let conn = self.read()?;
        let mut stmt = conn.prepare("SELECT hash FROM torrents ORDER BY hash LIMIT ?1")?;
        let hashes = stmt
            .query_map([limit], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(hashes)
    }
}

/// Return the hash spelling stored in the cache for a logical torrent.
/// Protocols such as qBittorrent accept hash values case-insensitively, while
/// the original cache schema uses a binary primary key.  Callers performing a
/// write must use this value for foreign-keyed tables and exact-key deletes.
pub(crate) fn canonical_hash(conn: &rusqlite::Connection, hash: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT hash FROM torrents WHERE hash=?1 COLLATE NOCASE",
            params![hash],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn current_revision_locked(conn: &Connection) -> Result<i64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            params![CACHE_REVISION_KEY],
            |row| row.get(0),
        )
        .optional()?;
    value
        .unwrap_or_else(|| "0".to_owned())
        .parse::<i64>()
        .context("parse cache revision")
}

pub(crate) fn allocate_revision(conn: &Connection) -> Result<i64> {
    let current = current_revision_locked(conn)?;
    let revision = current
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("cache revision exhausted"))?;
    conn.execute(
        "INSERT INTO kv(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![CACHE_REVISION_KEY, revision.to_string()],
    )?;
    Ok(revision)
}

pub(crate) fn prune_removed_tombstones(conn: &Connection, current_revision: i64) -> Result<()> {
    let cutoff = current_revision.saturating_sub(MAX_REMOVED_TORRENT_TOMBSTONES);
    conn.execute(
        "DELETE FROM removed_torrents WHERE revision <= ?1",
        params![cutoff],
    )?;

    let floor = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            params![CACHE_REVISION_FLOOR_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse::<i64>())
        .transpose()
        .context("parse cache revision floor")?
        .unwrap_or(0);
    if cutoff > floor {
        conn.execute(
            "INSERT INTO kv(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![CACHE_REVISION_FLOOR_KEY, cutoff.to_string()],
        )?;
    }
    Ok(())
}

/// Registers `tng_media_type_match(name, category, directory, tags, media_type)`
/// so SQL queries can classify torrents with the same word-boundary-aware
/// logic used everywhere else, instead of raw `LIKE '%...%'` globs (which
/// are both imprecise and, for season/episode globs, catastrophically so).
fn register_media_type_function(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "tng_media_type_match",
        5,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let name = ctx.get::<String>(0)?;
            let category = ctx.get::<String>(1)?;
            let directory = ctx.get::<String>(2)?;
            let tags = ctx.get::<String>(3)?;
            let media_type = ctx.get::<String>(4)?;
            Ok(crate::media_type::matches(
                &name,
                &category,
                &directory,
                &tags,
                &media_type,
            ))
        },
    )?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS torrents (
            hash                TEXT PRIMARY KEY,
            name                TEXT NOT NULL,
            size_bytes          INTEGER NOT NULL DEFAULT 0,
            bytes_done          INTEGER NOT NULL DEFAULT 0,
            down_rate           INTEGER NOT NULL DEFAULT 0,
            up_rate             INTEGER NOT NULL DEFAULT 0,
            up_total            INTEGER NOT NULL DEFAULT 0,
            down_total          INTEGER NOT NULL DEFAULT 0,
            ratio               INTEGER NOT NULL DEFAULT 0,
            is_active           INTEGER NOT NULL DEFAULT 0,
            is_open             INTEGER NOT NULL DEFAULT 0,
            complete            INTEGER NOT NULL DEFAULT 0,
            state               INTEGER NOT NULL DEFAULT 0,
            priority            INTEGER NOT NULL DEFAULT 0,
            category            TEXT NOT NULL DEFAULT '',
            base_path           TEXT NOT NULL DEFAULT '',
            directory           TEXT NOT NULL DEFAULT '',
            creation_date       INTEGER NOT NULL DEFAULT 0,
            timestamp_finished  INTEGER NOT NULL DEFAULT 0,
            tracker_focus       INTEGER NOT NULL DEFAULT 0,
            peers_connected     INTEGER NOT NULL DEFAULT 0,
            peers_complete      INTEGER NOT NULL DEFAULT 0,
            message             TEXT NOT NULL DEFAULT '',
            tracker_url         TEXT NOT NULL DEFAULT '',
            tags                TEXT NOT NULL DEFAULT '',
            updated_at          INTEGER NOT NULL DEFAULT 0,
            revision            INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_torrents_status   ON torrents(is_active, complete);
        CREATE INDEX IF NOT EXISTS idx_torrents_category ON torrents(category);
        CREATE INDEX IF NOT EXISTS idx_torrents_name     ON torrents(name COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS idx_torrents_updated_at ON torrents(updated_at);

        CREATE TABLE IF NOT EXISTS removed_torrents (
            hash     TEXT PRIMARY KEY,
            revision INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_removed_torrents_revision
            ON removed_torrents(revision);

        CREATE TABLE IF NOT EXISTS categories (
            name      TEXT PRIMARY KEY,
            save_path TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS tags (
            name TEXT PRIMARY KEY
        );

        CREATE TABLE IF NOT EXISTS torrent_tags (
            hash TEXT NOT NULL REFERENCES torrents(hash) ON DELETE CASCADE,
            tag  TEXT NOT NULL REFERENCES tags(name)     ON DELETE CASCADE,
            PRIMARY KEY (hash, tag)
        );

        CREATE TABLE IF NOT EXISTS torrent_tag_totals (
            singleton         INTEGER PRIMARY KEY CHECK (singleton=1),
            assignment_count  INTEGER NOT NULL,
            assignment_bytes  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS torrent_tag_stats (
            hash              TEXT PRIMARY KEY,
            assignment_count  INTEGER NOT NULL,
            assignment_bytes  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS kv (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS app_events (
            event_id    INTEGER PRIMARY KEY AUTOINCREMENT,
            occurred_at INTEGER NOT NULL,
            level       TEXT NOT NULL DEFAULT 'info',
            kind        TEXT NOT NULL,
            message     TEXT NOT NULL DEFAULT '',
            payload     TEXT NOT NULL DEFAULT '{}'
        );

        CREATE INDEX IF NOT EXISTS idx_app_events_kind ON app_events(kind);
    ",
    )?;

    // The compatible-client service cache predates the durable revision column. Keep migration
    // idempotent for existing installations instead of assuming a fresh DB.
    let has_revision = conn
        .prepare("PRAGMA table_info(torrents)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .any(|name| name == "revision");
    if !has_revision {
        conn.execute(
            "ALTER TABLE torrents ADD COLUMN revision INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_torrents_revision ON torrents(revision)",
        [],
    )?;

    // A pre-fix compatible-client service could persist the same logical hash with different
    // casing because the legacy primary key used binary collation. Collapse
    // those rows before new case-insensitive upserts start resolving them;
    // otherwise one spelling would remain invisible to reads and tombstone
    // replay could report a phantom duplicate.
    collapse_case_duplicate_hashes(conn)?;
    rebuild_torrent_tag_stats(conn)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    let revision = conn
        .query_row(
            "SELECT value FROM kv WHERE key=?1",
            params![CACHE_REVISION_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse::<i64>())
        .transpose()
        .context("parse persisted cache revision")?
        .unwrap_or(0);
    let revision = revision.max(now).max(1);
    conn.execute(
        "INSERT INTO kv(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![CACHE_REVISION_KEY, revision.to_string()],
    )?;

    let floor_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM kv WHERE key=?1)",
        params![CACHE_REVISION_FLOOR_KEY],
        |row| row.get::<_, i64>(0).map(|value| value != 0),
    )?;
    if !floor_exists {
        // Existing rows were created before revision tracking and have no
        // replayable history. Force old cursors through a fresh full sync;
        // a full response is the only truthful reconstruction.
        conn.execute(
            "INSERT INTO kv(key, value) VALUES(?1, ?2)",
            params![CACHE_REVISION_FLOOR_KEY, revision.to_string()],
        )?;
    }
    Ok(())
}

fn rebuild_torrent_tag_stats(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    // The base table remains authoritative. Rebuild once at startup so an
    // interrupted older process or a pre-migration database cannot leave the
    // O(1) capacity summaries stale.
    tx.execute("DELETE FROM torrent_tag_stats", [])?;
    tx.execute(
        "INSERT INTO torrent_tag_stats(hash, assignment_count, assignment_bytes)
         SELECT hash, COUNT(*), COALESCE(SUM(length(CAST(tag AS BLOB))), 0)
         FROM torrent_tags GROUP BY hash",
        [],
    )?;
    tx.execute(
        "INSERT INTO torrent_tag_totals(singleton, assignment_count, assignment_bytes)
         VALUES (
             1,
             (SELECT COUNT(*) FROM torrent_tags),
             (SELECT COALESCE(SUM(length(CAST(tag AS BLOB))), 0) FROM torrent_tags)
         )
         ON CONFLICT(singleton) DO UPDATE SET
             assignment_count=excluded.assignment_count,
             assignment_bytes=excluded.assignment_bytes",
        [],
    )?;
    tx.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS torrent_tags_stats_after_insert
        AFTER INSERT ON torrent_tags
        BEGIN
            UPDATE torrent_tag_totals
            SET assignment_count=assignment_count + 1,
                assignment_bytes=assignment_bytes + length(CAST(NEW.tag AS BLOB))
            WHERE singleton=1;
            INSERT INTO torrent_tag_stats(hash, assignment_count, assignment_bytes)
            VALUES (NEW.hash, 1, length(CAST(NEW.tag AS BLOB)))
            ON CONFLICT(hash) DO UPDATE SET
                assignment_count=assignment_count + 1,
                assignment_bytes=assignment_bytes + excluded.assignment_bytes;
        END;

        CREATE TRIGGER IF NOT EXISTS torrent_tags_stats_after_delete
        AFTER DELETE ON torrent_tags
        BEGIN
            UPDATE torrent_tag_totals
            SET assignment_count=MAX(assignment_count - 1, 0),
                assignment_bytes=MAX(assignment_bytes - length(CAST(OLD.tag AS BLOB)), 0)
            WHERE singleton=1;
            UPDATE torrent_tag_stats
            SET assignment_count=MAX(assignment_count - 1, 0),
                assignment_bytes=MAX(assignment_bytes - length(CAST(OLD.tag AS BLOB)), 0)
            WHERE hash=OLD.hash;
            DELETE FROM torrent_tag_stats
            WHERE hash=OLD.hash AND assignment_count=0;
        END;

        CREATE TRIGGER IF NOT EXISTS torrent_tags_stats_after_update
        AFTER UPDATE OF hash, tag ON torrent_tags
        BEGIN
            UPDATE torrent_tag_totals
            SET assignment_bytes=MAX(assignment_bytes - length(CAST(OLD.tag AS BLOB)), 0)
                                  + length(CAST(NEW.tag AS BLOB))
            WHERE singleton=1;
            UPDATE torrent_tag_stats
            SET assignment_count=MAX(assignment_count - 1, 0),
                assignment_bytes=MAX(assignment_bytes - length(CAST(OLD.tag AS BLOB)), 0)
            WHERE hash=OLD.hash;
            DELETE FROM torrent_tag_stats
            WHERE hash=OLD.hash AND assignment_count=0;
            INSERT INTO torrent_tag_stats(hash, assignment_count, assignment_bytes)
            VALUES (NEW.hash, 1, length(CAST(NEW.tag AS BLOB)))
            ON CONFLICT(hash) DO UPDATE SET
                assignment_count=assignment_count + 1,
                assignment_bytes=assignment_bytes + excluded.assignment_bytes;
        END;
        ",
    )?;
    tx.commit()?;
    Ok(())
}

fn collapse_case_duplicate_hashes(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;

    let torrent_hashes = {
        let mut stmt = tx.prepare("SELECT hash FROM torrents ORDER BY lower(hash), hash")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut torrent_groups = BTreeMap::<String, Vec<String>>::new();
    for hash in torrent_hashes {
        torrent_groups
            .entry(hash.to_ascii_lowercase())
            .or_default()
            .push(hash);
    }
    for hashes in torrent_groups.values().filter(|hashes| hashes.len() > 1) {
        let canonical = &hashes[0];
        for duplicate in &hashes[1..] {
            // Preserve the union of labels before deleting the duplicate
            // row. `tags` may be an older table without the current foreign
            // key, so make the referenced label explicit before inserting.
            tx.execute(
                "INSERT OR IGNORE INTO tags(name)
                 SELECT tag FROM torrent_tags WHERE hash=?1",
                params![duplicate],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO torrent_tags(hash, tag)
                 SELECT ?1, tag FROM torrent_tags WHERE hash=?2",
                params![canonical, duplicate],
            )?;
            tx.execute("DELETE FROM torrent_tags WHERE hash=?1", params![duplicate])?;
            tx.execute("DELETE FROM torrents WHERE hash=?1", params![duplicate])?;
        }
    }

    let removed_rows = {
        let mut stmt =
            tx.prepare("SELECT hash, revision FROM removed_torrents ORDER BY lower(hash), hash")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut removed_winners = BTreeMap::<String, (String, i64)>::new();
    for (hash, revision) in &removed_rows {
        let key = hash.to_ascii_lowercase();
        let replace = removed_winners
            .get(&key)
            .is_none_or(|(_, current_revision)| revision > current_revision);
        if replace {
            removed_winners.insert(key, (hash.clone(), *revision));
        }
    }
    for (hash, _) in &removed_rows {
        tx.execute("DELETE FROM removed_torrents WHERE hash=?1", params![hash])?;
    }
    for (_, (hash, revision)) in removed_winners {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE hash=?1 COLLATE NOCASE)",
            params![hash.as_str()],
            |row| row.get::<_, i64>(0).map(|value| value != 0),
        )?;
        if !live {
            tx.execute(
                "INSERT INTO removed_torrents(hash, revision) VALUES(?1, ?2)",
                params![hash, revision],
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

fn prune_app_events_locked(conn: &Connection, retention: usize) -> Result<()> {
    conn.execute(
        "DELETE FROM app_events
         WHERE event_id NOT IN (
             SELECT event_id FROM app_events ORDER BY event_id DESC LIMIT ?1
         )",
        params![retention as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::query::ListParams;

    fn poison_mutex<T: Send>(mutex: &Mutex<T>) {
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = mutex.lock().expect("mutex starts unpoisoned");
                std::panic::panic_any("cache-mutex-poison-test");
            });
            assert!(handle.join().is_err());
        });
    }

    #[test]
    fn poisoned_writer_fails_closed_without_repeated_panics() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        poison_mutex(&db.0.writer);

        let error = match db.conn() {
            Ok(_) => panic!("poisoned writer unexpectedly remained available"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "cache writer connection is unavailable after mutex poisoning"
        );
        assert!(db.0.writer.is_poisoned());
        assert!(db.conn().is_err());
    }

    #[test]
    fn poisoned_readers_are_skipped_and_all_poisoned_pool_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        poison_mutex(&db.0.readers.conns[0]);
        let conn = db.read().expect("remaining reader connections stay usable");
        let one: i64 = conn
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("healthy reader can execute queries");
        assert_eq!(one, 1);
        drop(conn);
        assert!(db.0.readers.conns[0].is_poisoned());

        for reader in db.0.readers.conns.iter().skip(1) {
            poison_mutex(reader);
        }
        let error = match db.read() {
            Ok(_) => panic!("fully poisoned reader pool unexpectedly remained available"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "cache read connection pool has no healthy connections"
        );
    }

    #[tokio::test]
    async fn blocking_task_error_does_not_retain_panic_payload() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let error = db
            .run_blocking("panic_probe", |_| -> Result<()> {
                std::panic::panic_any("cache-blocking-panic-secret");
            })
            .await
            .expect_err("panicking blocking task should fail");
        let rendered = format!("{error:#}");
        assert_eq!(rendered, "cache blocking task failed: panic_probe panicked");
        assert!(!rendered.contains("cache-blocking-panic-secret"));
    }

    /// TEMP tables are connection-local in SQLite -- a `CREATE TEMP TABLE`
    /// on one `Connection` is invisible to every other `Connection`, even
    /// against the same file. Creating one on the writer and failing to
    /// see it from a checked-out read-pool connection proves `read()`
    /// hands out a genuinely separate connection, not the writer's
    /// connection wrapped behind a second accessor.
    #[test]
    fn read_pool_connections_are_independent_from_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        db.conn()
            .expect("healthy test writer")
            .execute_batch("CREATE TEMP TABLE writer_only(x INTEGER);")
            .unwrap();

        let visible: i64 = db
            .read()
            .expect("healthy test reader")
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master WHERE name='writer_only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            visible, 0,
            "a read-pool connection must not share the writer's connection"
        );
    }

    #[test]
    fn bounded_hash_read_stops_at_the_requested_row_limit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        for hash in ["bounded-a", "bounded-b", "bounded-c"] {
            db.upsert(&torrent_row(hash, 0, false, false)).unwrap();
        }

        let hashes = db.all_hashes_bounded(2).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    /// Demonstrates the actual point of the read pool: two reads issued at
    /// the same time run concurrently instead of queuing behind one shared
    /// connection/mutex. Each thread holds a `read()` guard for a fixed
    /// sleep; if reads serialized (the old single-`Mutex<Connection>`
    /// behavior), total wall time would be roughly additive (~2x one
    /// sleep). With independent pooled connections it should be roughly
    /// one sleep.
    #[test]
    fn concurrent_reads_overlap_instead_of_serializing() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let hold_for = std::time::Duration::from_millis(200);

        let start = std::time::Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let db = db.clone();
                scope.spawn(move || {
                    let _guard = db.read().expect("healthy test reader");
                    std::thread::sleep(hold_for);
                });
            }
        });
        let elapsed = start.elapsed();

        assert!(
            elapsed < hold_for * 2,
            "two concurrent reads took {elapsed:?}; expected them to overlap \
             (~{hold_for:?}) instead of serializing to ~{:?}",
            hold_for * 2
        );
    }

    /// The writer stays a single serialized connection (WAL allows exactly
    /// one writer): concurrent writers must not lose updates to each
    /// other. This is distinct from the read-pool tests above and guards
    /// against accidentally routing a write through `read()`.
    #[test]
    fn concurrent_writes_via_conn_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let start = Arc::new(std::sync::Barrier::new(8));

        std::thread::scope(|scope| {
            for i in 0..8 {
                let db = db.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    db.set_kv(&format!("key-{i}"), "value").unwrap();
                });
            }
        });

        for i in 0..8 {
            assert_eq!(
                db.get_kv(&format!("key-{i}")).unwrap().as_deref(),
                Some("value")
            );
        }
    }

    #[test]
    fn app_events_insert_list_and_prune() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        for i in 0..3 {
            db.append_app_event(
                &AppEventRow {
                    event_id: None,
                    occurred_at: i,
                    level: "info".to_owned(),
                    kind: "test".to_owned(),
                    message: format!("event {i}"),
                    payload: "{}".to_owned(),
                },
                2,
            )
            .unwrap();
        }
        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].message, "event 2");
        assert_eq!(events[1].message, "event 1");
    }

    #[test]
    fn app_events_filter_before_limit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.append_app_event(
            &AppEventRow {
                event_id: None,
                occurred_at: 1,
                level: "warn".to_owned(),
                kind: "rtorrent_log".to_owned(),
                message: "old warning".to_owned(),
                payload: "{}".to_owned(),
            },
            10,
        )
        .unwrap();
        for i in 0..3 {
            db.append_app_event(
                &AppEventRow {
                    event_id: None,
                    occurred_at: i + 2,
                    level: "info".to_owned(),
                    kind: "sync".to_owned(),
                    message: format!("new info {i}"),
                    payload: "{}".to_owned(),
                },
                10,
            )
            .unwrap();
        }

        let events = db
            .list_app_events_filtered(1, Some("rtorrent_log"), &["warn"], None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "old warning");

        let events = db.list_app_events_filtered(10, None, &[], Some(1)).unwrap();
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|event| event.event_id.unwrap_or_default() > 1));
    }

    #[test]
    fn app_events_reject_invalid_payload_json() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let error = db
            .append_app_event(
                &AppEventRow {
                    event_id: None,
                    occurred_at: 0,
                    level: "info".to_owned(),
                    kind: "test".to_owned(),
                    message: "event".to_owned(),
                    payload: "not json".to_owned(),
                },
                10,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("validate app event payload JSON"));
    }

    #[test]
    fn app_events_redact_sensitive_messages_and_payloads_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let message = "failed https://log-user:log-password@tracker.example/path-passkey?signature=log-query-secret /srv/private/log-file.bin\npasskey=split-line-secret \u{202e}spoof";
        let payload = serde_json::json!({
            "error": "request to https://payload-user:payload-password@tracker.example/private?token=payload-url-secret failed",
            "credentials": {
                "access_token": "payload-access-secret",
                "safe_label": "ordinary"
            },
            "path": "/srv/private/payload-file.bin"
        })
        .to_string();
        db.append_app_event(
            &AppEventRow {
                event_id: None,
                occurred_at: 1,
                level: "error".to_owned(),
                kind: "test".to_owned(),
                message: message.to_owned(),
                payload,
            },
            10,
        )
        .unwrap();

        let conn = db.conn().expect("healthy test writer");
        let (stored_message, stored_payload): (String, String) = conn
            .query_row(
                "SELECT message, payload FROM app_events ORDER BY event_id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        drop(conn);
        let stored_rendered = format!("{stored_message} {stored_payload}");
        for secret in [
            "log-user",
            "log-password",
            "path-passkey",
            "log-query-secret",
            "split-line-secret",
            "/srv/private",
            "payload-user",
            "payload-password",
            "payload-url-secret",
            "payload-access-secret",
        ] {
            assert!(!stored_rendered.contains(secret), "{stored_rendered}");
        }
        assert!(!stored_message.chars().any(char::is_control));

        let event = db.list_app_events(10).unwrap().remove(0);
        let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(payload["credentials"]["access_token"], "[redacted]");
        assert_eq!(payload["credentials"]["safe_label"], "ordinary");
        assert!(event.message.contains("https://tracker.example/"));
        assert!(event.message.contains("[redacted-path:log-file.bin]"));
        assert!(event.message.contains("passkey=[redacted]"));
        assert!(!event.message.chars().any(char::is_control));

        let rendered = format!("{} {payload}", event.message);
        for secret in [
            "log-user",
            "log-password",
            "path-passkey",
            "log-query-secret",
            "split-line-secret",
            "/srv/private",
            "payload-user",
            "payload-password",
            "payload-url-secret",
            "payload-access-secret",
        ] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
    }

    #[test]
    fn app_events_redact_legacy_sensitive_messages_and_payloads_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let raw_message = "legacy https://old-user:old-password@tracker.example/old-passkey?signature=old-query-secret /var/private/legacy.bin\u{001b}[31m";
        let raw_payload = serde_json::json!({
            "error": "old response https://payload-old-user:payload-old-password@tracker.example/private?token=payload-old-url-secret",
            "api_key": "payload-old-api-secret",
            "path": "/var/private/legacy-payload.bin"
        })
        .to_string();
        let conn = db.conn().expect("healthy test writer");
        conn.execute(
            "INSERT INTO app_events (occurred_at, level, kind, message, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![1_i64, "warn", "legacy", raw_message, raw_payload],
        )
        .unwrap();
        drop(conn);

        let event = db.list_app_events(10).unwrap().remove(0);
        let payload: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
        assert_eq!(payload["api_key"], "[redacted]");
        assert!(event.message.contains("https://tracker.example/"));
        assert!(event.message.contains("[redacted-path:"));
        assert!(!event.message.chars().any(char::is_control));

        let rendered = format!("{} {payload}", event.message);
        for secret in [
            "old-user",
            "old-password",
            "old-passkey",
            "old-query-secret",
            "payload-old-user",
            "payload-old-password",
            "payload-old-url-secret",
            "payload-old-api-secret",
            "/var/private",
        ] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
    }

    #[test]
    fn legacy_cache_migration_adds_revision_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE torrents (
                    hash TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    is_active INTEGER NOT NULL DEFAULT 0,
                    complete INTEGER NOT NULL DEFAULT 0,
                    category TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        let conn = db.conn().expect("healthy test writer");
        let columns = conn
            .prepare("PRAGMA table_info(torrents)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.iter().any(|name| name == "revision"));
        assert!(current_revision_locked(&conn).unwrap() > 0);
        assert!(
            conn.query_row("SELECT EXISTS(SELECT 1 FROM removed_torrents)", [], |row| {
                row.get::<_, i64>(0)
            },)
                .unwrap()
                == 0
        );
    }

    #[test]
    fn legacy_case_collisions_are_collapsed_with_tag_and_tombstone_union() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE torrents (
                    hash TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    is_active INTEGER NOT NULL DEFAULT 0,
                    complete INTEGER NOT NULL DEFAULT 0,
                    category TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE tags (name TEXT PRIMARY KEY);
                CREATE TABLE torrent_tags (
                    hash TEXT NOT NULL,
                    tag TEXT NOT NULL,
                    PRIMARY KEY(hash, tag)
                );
                CREATE TABLE removed_torrents (
                    hash TEXT PRIMARY KEY,
                    revision INTEGER NOT NULL
                );
                INSERT INTO torrents(hash, name) VALUES ('ABCDEF', 'old'), ('abcdef', 'new');
                INSERT INTO tags(name) VALUES ('alpha'), ('beta');
                INSERT INTO torrent_tags(hash, tag) VALUES ('ABCDEF', 'alpha');
                INSERT INTO torrent_tags(hash, tag) VALUES ('abcdef', 'beta');
                INSERT INTO removed_torrents(hash, revision) VALUES ('123ABC', 4), ('123abc', 9);
                INSERT INTO removed_torrents(hash, revision) VALUES ('ABCDEF', 12);",
            )
            .unwrap();
        }

        let db = Db::open(&path).unwrap();
        let conn = db.conn().expect("healthy test writer");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM torrents", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT GROUP_CONCAT(tag) FROM (
                    SELECT tag FROM torrent_tags ORDER BY tag
                )",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "alpha,beta"
        );
        assert_eq!(
            conn.query_row(
                "SELECT hash || ':' || revision FROM removed_torrents",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "123abc:9"
        );
    }

    #[test]
    fn stopped_and_queued_statuses_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&torrent_row("queued", 1, false, false)).unwrap();
        // "stalled" means started (is_active=1) but zero throughput right
        // now (down_rate=0, set by torrent_row) -- not is_active=0, which
        // means stopped.
        db.upsert(&torrent_row("stalled", 1, true, true)).unwrap();
        db.upsert(&torrent_row("stopped", 0, false, false)).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.status.get("queued"), Some(&1));
        assert_eq!(facets.status.get("stopped"), Some(&1));
        assert_eq!(facets.status.get("stalled"), Some(&1));
        assert_eq!(facets.status.get("inactive"), Some(&2));

        let (queued, _) = db
            .list(&ListParams {
                status: Some("queued".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].hash, "queued");

        let (stopped, _) = db
            .list(&ListParams {
                status: Some("stopped".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].hash, "stopped");
    }

    #[test]
    fn torrent_hash_operations_are_case_insensitive_without_duplicate_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&torrent_row("ABCDEF1234567890", 1, true, true))
            .unwrap();

        let mut refreshed = torrent_row("abcdef1234567890", 1, true, true);
        refreshed.name = "Refreshed".to_owned();
        db.upsert(&refreshed).unwrap();
        assert_eq!(db.count().unwrap(), 1);
        assert_eq!(
            db.get("abcdef1234567890").unwrap().unwrap().name,
            "Refreshed"
        );

        db.set_torrent_tags("abcdef1234567890", &["one", "two"])
            .unwrap();
        db.set_torrent_category("abcdef1234567890", "Movies")
            .unwrap();
        db.set_torrent_location("abcdef1234567890", "/data/movies")
            .unwrap();
        db.set_torrent_runtime_state("abcdef1234567890", 0, false, false)
            .unwrap();

        let row = db.get("ABCDEF1234567890").unwrap().unwrap();
        assert_eq!(row.hash, "ABCDEF1234567890");
        assert_eq!(row.tags, "one,two");
        assert_eq!(row.category, "Movies");
        assert_eq!(row.directory, "/data/movies");
        assert!(!row.is_active);
        assert!(!row.is_open);

        db.delete("abcdef1234567890").unwrap();
        assert!(!db.exists("ABCDEF1234567890").unwrap());
        assert!(db.get("abcdef1234567890").unwrap().is_none());
        let conn = db.conn().expect("healthy test writer");
        assert_eq!(
            conn.query_row("SELECT hash FROM removed_torrents", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "ABCDEF1234567890"
        );
    }

    #[test]
    fn set_torrent_runtime_state_many_updates_every_row_in_one_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        for i in 0..50 {
            db.upsert(&torrent_row(&format!("h{i}"), 1, true, true))
                .unwrap();
        }

        let updates: Vec<(String, i64, bool, bool)> = (0..50)
            .map(|i| (format!("h{i}"), 0i64, false, false))
            .collect();
        db.set_torrent_runtime_state_many(&updates).unwrap();

        let (stopped, total) = db
            .list(&ListParams {
                status: Some("stopped".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 50, "every row must have been updated, not just some");
        assert_eq!(stopped.len(), 50);
    }

    #[test]
    fn runtime_state_change_invalidates_previous_rate_sample() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut torrent = torrent_row("rate-reset", 1, true, true);
        torrent.down_rate = 12_345;
        torrent.up_rate = 678;
        torrent.updated_at = 1;
        db.upsert(&torrent).unwrap();

        db.set_torrent_runtime_state("rate-reset", 0, false, false)
            .unwrap();

        let row = db.get("rate-reset").unwrap().unwrap();
        assert_eq!(row.down_rate, 0);
        assert_eq!(row.up_rate, 0);
        assert!(!row.is_active);
    }

    #[test]
    fn set_torrent_runtime_state_many_empty_input_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.set_torrent_runtime_state_many(&[]).unwrap();
    }

    #[test]
    fn sync_counts_match_backend_counter_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        let mut errored = torrent_row("errored", 3, true, true);
        errored.message = "tracker failure".to_owned();
        errored.peers_connected = 2;
        db.upsert(&errored).unwrap();

        let mut stopped = torrent_row("stopped", 0, false, false);
        stopped.message = "non-terminal warning".to_owned();
        stopped.peers_connected = 3;
        db.upsert(&stopped).unwrap();

        let mut seeding = torrent_row("seeding", 1, true, true);
        seeding.complete = true;
        seeding.peers_connected = 5;
        db.upsert(&seeding).unwrap();

        let mut downloading = torrent_row("downloading", 1, true, true);
        downloading.peers_connected = 7;
        db.upsert(&downloading).unwrap();

        assert_eq!(db.sync_counts().unwrap(), (1, 1, 1, 1, 17));
    }

    #[test]
    fn unchanged_upsert_refreshes_last_seen_without_advancing_revision() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut first = torrent_row("same", 1, true, true);
        first.updated_at = 10;
        db.upsert(&first).unwrap();
        let revision = db.current_revision().unwrap();

        let mut refreshed = first.clone();
        refreshed.updated_at = 20;
        db.upsert(&refreshed).unwrap();

        assert_eq!(db.current_revision().unwrap(), revision);
        assert_eq!(db.get("same").unwrap().unwrap().updated_at, 20);
        assert!(db
            .list_since_bounded(revision, 10)
            .unwrap()
            .unwrap()
            .changed
            .is_empty());
    }

    #[test]
    fn tag_capable_upsert_reconciles_remote_tags_and_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut torrent = torrent_row("tagged", 1, true, true);
        torrent.tags = " Movies, tv, Movies ".to_owned();
        db.upsert_with_tags(&torrent, true).unwrap();
        assert_eq!(db.get("tagged").unwrap().unwrap().tags, "Movies,tv");

        let revision = db.current_revision().unwrap();
        torrent.tags = "tv".to_owned();
        db.upsert_with_tags(&torrent, true).unwrap();
        assert!(db.current_revision().unwrap() > revision);
        assert_eq!(db.get("tagged").unwrap().unwrap().tags, "tv");

        torrent.tags.clear();
        db.upsert_with_tags(&torrent, true).unwrap();
        assert!(db.get("tagged").unwrap().unwrap().tags.is_empty());
    }

    #[test]
    fn label_dictionaries_and_backend_tag_projection_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        {
            let mut conn = db.conn().expect("healthy test writer");
            let tx = conn.transaction().unwrap();
            for index in 0..crate::cache::categories::MAX_CACHED_LABEL_DICTIONARY_ITEMS {
                tx.execute(
                    "INSERT INTO categories(name, save_path) VALUES(?1, '')",
                    params![format!("category-{index:05}")],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO tags(name) VALUES(?1)",
                    params![format!("tag-{index:05}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        let tag_error = db.ensure_tag("one-more-tag").unwrap_err();
        assert!(tag_error
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
        let category_error = db
            .upsert_category("one-more-category", "/downloads")
            .unwrap_err();
        assert!(category_error
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());

        let mut torrent = torrent_row("oversized-tag-projection", 1, true, true);
        torrent.tags = "x".repeat(MAX_CACHED_LABEL_NAME_BYTES + 1);
        assert!(db.upsert_with_tags(&torrent, true).is_err());
        torrent.tags.clear();
        torrent.category = "x".repeat(MAX_CACHED_LABEL_NAME_BYTES + 1);
        assert!(db.upsert(&torrent).is_err());

        {
            let conn = db.conn().expect("healthy test writer");
            conn.execute("INSERT INTO tags(name) VALUES('legacy-extra-tag')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO categories(name, save_path) VALUES('legacy-extra-category', '')",
                [],
            )
            .unwrap();
        }
        assert!(db
            .list_tags()
            .unwrap_err()
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
        assert!(db
            .list_categories()
            .unwrap_err()
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
    }

    #[test]
    fn per_torrent_tag_assignment_and_legacy_label_rows_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let torrent = torrent_row("bounded-tag-list", 1, true, true);
        db.upsert(&torrent).unwrap();

        let tags = (0..MAX_CACHED_TAGS_PER_MUTATION)
            .map(|index| format!("tag-{index:04}"))
            .collect::<Vec<_>>();
        let tag_refs = tags.iter().map(String::as_str).collect::<Vec<_>>();
        db.add_torrent_tags("bounded-tag-list", &tag_refs).unwrap();
        assert_eq!(
            db.get_torrent_tags("bounded-tag-list").unwrap().len(),
            MAX_CACHED_TAGS_PER_MUTATION
        );

        let error = db
            .add_torrent_tag("bounded-tag-list", "one-more")
            .unwrap_err();
        assert!(error
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
        assert!(!db.list_tags().unwrap().iter().any(|tag| tag == "one-more"));

        {
            let conn = db.conn().expect("healthy test writer");
            conn.execute("INSERT INTO tags(name) VALUES('legacy-extra')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO torrent_tags(hash, tag) VALUES('bounded-tag-list', 'legacy-extra')",
                [],
            )
            .unwrap();
        }
        let error = db.get("bounded-tag-list").unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(&error));
        let error = db
            .list_page(&crate::cache::query::ListParams::default())
            .unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(&error));

        {
            let conn = db.conn().expect("healthy test writer");
            conn.execute(
                "INSERT INTO tags(name) VALUES(?1)",
                params!["x".repeat(MAX_CACHED_TAG_MUTATION_BYTES + 1)],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO categories(name, save_path) VALUES('oversized-path', ?1)",
                params!["x".repeat(MAX_CACHED_TAG_MUTATION_BYTES + 1)],
            )
            .unwrap();
        }
        assert!(db
            .list_tags()
            .unwrap_err()
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
        assert!(db
            .list_categories()
            .unwrap_err()
            .downcast_ref::<crate::cache::categories::CategoryTagCapacityError>()
            .is_some());
    }

    #[test]
    fn tag_assignment_summaries_track_mutations_cascades_and_startup_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cache.db");
        let db = Db::open(&db_path).unwrap();
        db.upsert(&torrent_row("tag-summary-a", 1, true, true))
            .unwrap();
        db.upsert(&torrent_row("tag-summary-b", 1, true, true))
            .unwrap();
        db.set_torrent_tags("tag-summary-a", &["one", "two"])
            .unwrap();
        db.add_torrent_tag("tag-summary-b", "two").unwrap();

        {
            let conn = db.conn().expect("healthy test writer");
            assert_eq!(
                conn.query_row(
                    "SELECT assignment_count, assignment_bytes FROM torrent_tag_totals",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
                (3, 9)
            );
            assert_eq!(
                conn.query_row(
                    "SELECT assignment_count, assignment_bytes
                     FROM torrent_tag_stats WHERE hash='tag-summary-a'",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
                (2, 6)
            );

            conn.execute("INSERT INTO tags(name) VALUES('longer')", [])
                .unwrap();
            conn.execute(
                "UPDATE torrent_tags SET tag='longer'
                 WHERE hash='tag-summary-a' AND tag='one'",
                [],
            )
            .unwrap();
            assert_eq!(
                conn.query_row(
                    "SELECT assignment_count, assignment_bytes FROM torrent_tag_totals",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
                (3, 12)
            );
        }

        db.delete("tag-summary-a").unwrap();
        {
            let conn = db.conn().expect("healthy test writer");
            assert_eq!(
                conn.query_row(
                    "SELECT assignment_count, assignment_bytes FROM torrent_tag_totals",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
                (1, 3)
            );
            conn.execute(
                "UPDATE torrent_tag_totals
                 SET assignment_count=999, assignment_bytes=999",
                [],
            )
            .unwrap();
            conn.execute("DELETE FROM torrent_tag_stats", []).unwrap();
        }

        drop(db);
        let reopened = Db::open(&db_path).unwrap();
        let conn = reopened.conn().expect("healthy test writer");
        assert_eq!(
            conn.query_row(
                "SELECT assignment_count, assignment_bytes FROM torrent_tag_totals",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
            (1, 3)
        );
        assert_eq!(
            conn.query_row(
                "SELECT assignment_count, assignment_bytes
                 FROM torrent_tag_stats WHERE hash='tag-summary-b'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
            (1, 3)
        );
    }

    #[test]
    fn over_limit_global_tag_assignments_block_list_projections_and_new_links() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&torrent_row("global-tag-cap", 1, true, true))
            .unwrap();
        db.add_torrent_tag("global-tag-cap", "existing").unwrap();
        let revision = db.current_revision().unwrap();
        {
            let conn = db.conn().expect("healthy test writer");
            conn.execute(
                "UPDATE torrent_tag_totals SET assignment_count=?1",
                params![(crate::cache::categories::MAX_CACHED_TAG_ASSIGNMENTS + 1) as i64],
            )
            .unwrap();
        }

        let mutation_error = db.add_torrent_tag("global-tag-cap", "blocked").unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(
            &mutation_error
        ));
        assert!(!db.list_tags().unwrap().iter().any(|tag| tag == "blocked"));

        let page_error = db.list_page(&ListParams::default()).unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(&page_error));
        let delta_error = db.list_since_bounded(revision, 10).unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(&delta_error));

        // A single-detail read remains bounded by the per-torrent SQL cap and
        // does not need to reject unrelated metadata outside its projection.
        assert_eq!(db.get("global-tag-cap").unwrap().unwrap().tags, "existing");
    }

    #[test]
    fn bulk_tag_capacity_preflight_counts_combined_hash_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&torrent_row("bulk-tag-a", 1, true, true))
            .unwrap();
        db.upsert(&torrent_row("bulk-tag-b", 1, true, true))
            .unwrap();
        {
            let conn = db.conn().expect("healthy test writer");
            conn.execute(
                "UPDATE torrent_tag_totals SET assignment_count=?1",
                params![(crate::cache::categories::MAX_CACHED_TAG_ASSIGNMENTS - 1) as i64],
            )
            .unwrap();
        }

        let hashes = vec!["bulk-tag-a".to_owned(), "bulk-tag-b".to_owned()];
        let error =
            crate::cache::categories::ensure_torrent_tag_capacities(&db, &hashes, &["one"], true)
                .unwrap_err();
        assert!(crate::cache::is_category_tag_capacity_error(&error));
        assert!(!db.list_tags().unwrap().iter().any(|tag| tag == "one"));
    }

    fn torrent_row(hash: &str, state: i64, is_active: bool, is_open: bool) -> TorrentRow {
        TorrentRow {
            hash: hash.to_owned(),
            name: hash.to_owned(),
            size_bytes: 100,
            bytes_done: 0,
            down_rate: 0,
            up_rate: 0,
            up_total: 0,
            down_total: 0,
            ratio: 0,
            is_active,
            is_open,
            complete: false,
            state,
            priority: 0,
            category: String::new(),
            base_path: "/downloads/test".to_owned(),
            directory: "/downloads/test".to_owned(),
            creation_date: 0,
            timestamp_finished: 0,
            tracker_focus: 0,
            peers_connected: 0,
            peers_complete: 0,
            message: String::new(),
            tracker_url: String::new(),
            tags: String::new(),
            updated_at: 0,
        }
    }
}
