use anyhow::{Context, Result};
use rusqlite::{params, Connection};
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
    ",
    )?;
    Ok(())
}
