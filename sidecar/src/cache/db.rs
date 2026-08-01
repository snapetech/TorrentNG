use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

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

#[derive(Clone)]
pub struct Db(pub(crate) Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cache dir {}", parent.display()))?;
        }

        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;

        migrate(&conn)?;

        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().expect("db mutex poisoned")
    }

    pub fn upsert(&self, t: &TorrentRow) -> Result<()> {
        self.conn()
            .execute(
                // tags is excluded from DO UPDATE — managed via torrent_tags table
                "INSERT INTO torrents (
                hash, name, size_bytes, bytes_done, down_rate, up_rate,
                up_total, down_total, ratio, is_active, is_open, complete,
                state, priority, category, base_path, directory, creation_date,
                timestamp_finished, tracker_focus, peers_connected, peers_complete,
                message, tracker_url, tags, updated_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                      ?17,?18,?19,?20,?21,?22,?23,?24,'',?25)
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
                updated_at=excluded.updated_at",
                params![
                    t.hash,
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
                ],
            )
            .context("upsert torrent")?;
        Ok(())
    }

    pub fn delete(&self, hash: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM torrents WHERE hash=?1", params![hash])?;
        Ok(())
    }

    pub fn append_app_event(&self, event: &AppEventRow, retention: usize) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO app_events (occurred_at, level, kind, message, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.occurred_at,
                event.level,
                event.kind,
                event.message,
                event.payload,
            ],
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
        let conn = self.conn();
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
            std::iter::repeat("?")
                .take(levels.len())
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
                Ok(AppEventRow {
                    event_id: Some(row.get(0)?),
                    occurred_at: row.get(1)?,
                    level: row.get(2)?,
                    kind: row.get(3)?,
                    message: row.get(4)?,
                    payload: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_kv(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM kv WHERE key=?1", params![key])?;
        Ok(())
    }

    pub fn exists(&self, hash: &str) -> Result<bool> {
        let exists: i64 = self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM torrents WHERE hash=?1)",
            params![hash],
            |r| r.get(0),
        )?;
        Ok(exists != 0)
    }

    pub fn count(&self) -> Result<i64> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM torrents", [], |r| r.get(0))?;
        Ok(n)
    }

    pub fn all_hashes(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT hash FROM torrents")?;
        let hashes = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(hashes)
    }
}

fn migrate(conn: &Connection) -> Result<()> {
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
            updated_at          INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_torrents_status   ON torrents(is_active, complete);
        CREATE INDEX IF NOT EXISTS idx_torrents_category ON torrents(category);
        CREATE INDEX IF NOT EXISTS idx_torrents_name     ON torrents(name COLLATE NOCASE);

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
    fn stopped_and_queued_statuses_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&torrent_row("queued", 1, false, false)).unwrap();
        db.upsert(&torrent_row("stalled", 1, false, true)).unwrap();
        db.upsert(&torrent_row("stopped", 0, false, false)).unwrap();

        let facets = db.sidebar_facets().unwrap();
        assert_eq!(facets.status.get("queued"), Some(&1));
        assert_eq!(facets.status.get("stopped"), Some(&1));
        assert_eq!(facets.status.get("stalled"), Some(&1));
        assert_eq!(facets.status.get("inactive"), Some(&3));

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
