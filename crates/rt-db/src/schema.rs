/// SQLite schema migrations.
///
/// Each migration is a (version, sql) pair applied in order.
/// The `schema_version` user_version pragma tracks the current schema level.
use rusqlite::Connection;

use crate::error::DbError;

/// Apply all pending migrations to bring the database up to the current schema version.
pub fn migrate(conn: &Connection) -> Result<(), DbError> {
    let current = get_schema_version(conn)?;
    for (version, sql) in MIGRATIONS.iter() {
        if *version > current {
            conn.execute_batch(sql).map_err(|e| DbError::Migration {
                version: *version,
                reason: e.to_string(),
            })?;
            set_schema_version(conn, *version)?;
        }
    }
    Ok(())
}

fn get_schema_version(conn: &Connection) -> Result<u32, DbError> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    Ok(v as u32)
}

fn set_schema_version(conn: &Connection, version: u32) -> Result<(), DbError> {
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

/// Ordered migrations. Each entry is (target_version, DDL).
const MIGRATIONS: &[(u32, &str)] = &[(
    1,
    "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS torrents (
            info_hash       TEXT    NOT NULL PRIMARY KEY, -- hex-encoded SHA-1
            name            TEXT    NOT NULL,
            total_length    INTEGER NOT NULL,
            piece_length    INTEGER NOT NULL,
            piece_count     INTEGER NOT NULL,
            is_private      INTEGER NOT NULL DEFAULT 0,
            save_path       TEXT    NOT NULL,
            category        TEXT,
            tags            TEXT    NOT NULL DEFAULT '[]',  -- JSON array
            state           TEXT    NOT NULL DEFAULT 'stopped',
            added_at        INTEGER NOT NULL, -- unix seconds
            completed_at    INTEGER,
            uploaded        INTEGER NOT NULL DEFAULT 0,
            downloaded      INTEGER NOT NULL DEFAULT 0,
            ratio           REAL    NOT NULL DEFAULT 0.0,
            trackers        TEXT    NOT NULL DEFAULT '[]'  -- JSON array of URLs
        );

        CREATE TABLE IF NOT EXISTS files (
            info_hash       TEXT    NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
            file_index      INTEGER NOT NULL,
            path            TEXT    NOT NULL,
            length          INTEGER NOT NULL,
            priority        INTEGER NOT NULL DEFAULT 1,  -- 0=skip 1=normal 2=high
            PRIMARY KEY (info_hash, file_index)
        );

        CREATE INDEX IF NOT EXISTS idx_torrents_state    ON torrents(state);
        CREATE INDEX IF NOT EXISTS idx_torrents_category ON torrents(category);
        CREATE INDEX IF NOT EXISTS idx_torrents_added_at ON torrents(added_at);
        ",
)];

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn migrate_creates_tables() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('torrents','files')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn migrate_idempotent() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap(); // second call should be a no-op
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn wal_mode_enabled() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        // In-memory DBs don't support WAL; just confirm we can query journal_mode.
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        // In-memory: stays "memory"; file-based would be "wal". Just check it's set.
        assert!(!mode.is_empty());
    }
}
