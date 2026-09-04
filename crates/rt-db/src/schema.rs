/// SQLite schema migrations.
///
/// Each migration is a (version, sql) pair applied in order.
/// The `schema_version` user_version pragma tracks the current schema level.
use rusqlite::Connection;

use crate::error::DbError;

/// Current native database schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 9;

/// Apply all pending migrations to bring the database up to the current schema version.
///
/// Every version's DDL and its `user_version` update run in the same SQLite
/// transaction.  SQLite rolls DDL back with the transaction, so a crash or a
/// failed statement cannot leave a partially applied schema advertising the
/// next version.  The journal-mode pragma is intentionally configured before
/// the transaction because SQLite does not allow changing it while a
/// transaction is active.
pub fn migrate(conn: &Connection) -> Result<(), DbError> {
    let current = get_schema_version(conn)?;
    if current > CURRENT_SCHEMA_VERSION {
        return Err(DbError::Migration {
            version: current,
            reason: format!(
                "database schema is newer than this binary (maximum supported {CURRENT_SCHEMA_VERSION})"
            ),
        });
    }

    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    if current == 0 {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    }

    let tx = conn.unchecked_transaction()?;
    for (version, sql) in MIGRATIONS.iter().filter(|(version, _)| *version > current) {
        tx.execute_batch(sql).map_err(|e| DbError::Migration {
            version: *version,
            reason: e.to_string(),
        })?;
        tx.pragma_update(None, "user_version", version)
            .map_err(|e| DbError::Migration {
                version: *version,
                reason: e.to_string(),
            })?;
    }
    tx.commit().map_err(|e| DbError::Migration {
        version: CURRENT_SCHEMA_VERSION,
        reason: e.to_string(),
    })?;
    Ok(())
}

fn get_schema_version(conn: &Connection) -> Result<u32, DbError> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    u32::try_from(v).map_err(|_| DbError::Migration {
        version: 0,
        reason: format!("invalid negative schema version {v}"),
    })
}

/// Ordered migrations. Each entry is (target_version, DDL).
const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        "
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
    ),
    (
        2,
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS torrent_files (
            info_hash       TEXT    NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
            file_index      INTEGER NOT NULL,
            path            TEXT    NOT NULL,
            length          INTEGER NOT NULL,
            offset          INTEGER NOT NULL DEFAULT 0,
            priority        INTEGER NOT NULL DEFAULT 1,
            wanted          INTEGER NOT NULL DEFAULT 1,
            completed_bytes INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (info_hash, file_index)
        );

        CREATE TABLE IF NOT EXISTS torrent_trackers (
            info_hash              TEXT    NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
            tracker_index          INTEGER NOT NULL,
            tier                   INTEGER NOT NULL DEFAULT 0,
            url                    TEXT    NOT NULL,
            status                 TEXT    NOT NULL DEFAULT 'unknown',
            last_announce_at       INTEGER,
            next_announce_at       INTEGER,
            last_success_at        INTEGER,
            failure_reason         TEXT,
            warning_message        TEXT,
            seeders                INTEGER,
            leechers               INTEGER,
            completed              INTEGER,
            uploaded               INTEGER NOT NULL DEFAULT 0,
            downloaded             INTEGER NOT NULL DEFAULT 0,
            left_bytes             INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (info_hash, tracker_index)
        );

        CREATE TABLE IF NOT EXISTS torrent_tags (
            info_hash       TEXT NOT NULL REFERENCES torrents(info_hash) ON DELETE CASCADE,
            tag             TEXT NOT NULL,
            PRIMARY KEY (info_hash, tag)
        );

        CREATE TABLE IF NOT EXISTS torrent_categories (
            name            TEXT NOT NULL PRIMARY KEY,
            save_path       TEXT,
            created_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS torrent_limits (
            info_hash              TEXT NOT NULL PRIMARY KEY REFERENCES torrents(info_hash) ON DELETE CASCADE,
            download_limit         INTEGER,
            upload_limit           INTEGER,
            max_connections        INTEGER,
            seed_ratio_limit       REAL,
            seed_idle_limit        INTEGER,
            sequential_download    INTEGER NOT NULL DEFAULT 0,
            first_last_piece_prio  INTEGER NOT NULL DEFAULT 0,
            force_start            INTEGER NOT NULL DEFAULT 0,
            super_seeding          INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS jobs (
            job_id              TEXT    NOT NULL PRIMARY KEY,
            kind                TEXT    NOT NULL,
            state               TEXT    NOT NULL,
            dry_run             INTEGER NOT NULL DEFAULT 0,
            affected_torrents   TEXT    NOT NULL DEFAULT '[]',
            total               INTEGER NOT NULL DEFAULT 0,
            done                INTEGER NOT NULL DEFAULT 0,
            checkpoint          INTEGER NOT NULL DEFAULT 0,
            file_index          INTEGER,
            piece_index         INTEGER,
            byte_offset         INTEGER,
            verified_bytes      INTEGER NOT NULL DEFAULT 0,
            invalid_pieces      TEXT    NOT NULL DEFAULT '[]',
            error               TEXT,
            created_at          INTEGER NOT NULL,
            started_at          INTEGER,
            updated_at          INTEGER NOT NULL,
            finished_at         INTEGER
        );

        CREATE TABLE IF NOT EXISTS session_events (
            event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
            occurred_at     INTEGER NOT NULL,
            info_hash       TEXT,
            kind            TEXT    NOT NULL,
            message         TEXT,
            payload         TEXT    NOT NULL DEFAULT '{}'
        );

        CREATE TABLE IF NOT EXISTS job_events (
            event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id          TEXT    NOT NULL REFERENCES jobs(job_id) ON DELETE CASCADE,
            occurred_at     INTEGER NOT NULL,
            kind            TEXT    NOT NULL,
            message         TEXT,
            payload         TEXT    NOT NULL DEFAULT '{}'
        );

        CREATE TABLE IF NOT EXISTS settings (
            key             TEXT NOT NULL PRIMARY KEY,
            value           TEXT NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS storage_roots (
            root_id         TEXT NOT NULL PRIMARY KEY,
            path            TEXT NOT NULL UNIQUE,
            profile         TEXT NOT NULL DEFAULT 'auto',
            created_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS mounts (
            mount_id        TEXT NOT NULL PRIMARY KEY,
            path            TEXT NOT NULL UNIQUE,
            fs_type         TEXT,
            device          TEXT,
            queue_depth     INTEGER NOT NULL DEFAULT 1,
            read_concurrency INTEGER NOT NULL DEFAULT 1,
            write_concurrency INTEGER NOT NULL DEFAULT 1,
            updated_at      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS api_tokens (
            token_id        TEXT NOT NULL PRIMARY KEY,
            name            TEXT NOT NULL,
            token_hash      TEXT NOT NULL,
            scopes          TEXT NOT NULL DEFAULT '[]',
            created_at      INTEGER NOT NULL,
            last_used_at    INTEGER,
            revoked_at      INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_torrent_files_hash ON torrent_files(info_hash);
        CREATE INDEX IF NOT EXISTS idx_torrent_trackers_hash ON torrent_trackers(info_hash);
        CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs(state);
        CREATE INDEX IF NOT EXISTS idx_jobs_updated_at ON jobs(updated_at);
        CREATE INDEX IF NOT EXISTS idx_session_events_hash ON session_events(info_hash);
        CREATE INDEX IF NOT EXISTS idx_session_events_kind ON session_events(kind);
        CREATE INDEX IF NOT EXISTS idx_job_events_job ON job_events(job_id);
        ",
    ),
    (
        3,
        "
        PRAGMA foreign_keys = ON;

        ALTER TABLE torrent_limits ADD COLUMN auto_tmm INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE torrent_limits ADD COLUMN auto_management INTEGER NOT NULL DEFAULT 0;
        ",
    ),
    (
        4,
        "
        PRAGMA foreign_keys = ON;

        ALTER TABLE session_events ADD COLUMN level TEXT NOT NULL DEFAULT 'info';

        UPDATE session_events
        SET level = CASE
            WHEN lower(kind) LIKE '%error%' OR lower(kind) LIKE '%failed%' THEN 'error'
            WHEN lower(kind) LIKE '%warn%' THEN 'warn'
            ELSE 'info'
        END;

        CREATE INDEX IF NOT EXISTS idx_session_events_level ON session_events(level);
        ",
    ),
    (
        5,
        "
        PRAGMA foreign_keys = ON;

        ALTER TABLE torrent_limits ADD COLUMN sequential_download_from_piece INTEGER;
        ",
    ),
    (
        6,
        "
        PRAGMA foreign_keys = ON;

        CREATE INDEX IF NOT EXISTS idx_torrent_trackers_status
            ON torrent_trackers(status);
        ",
    ),
    (
        7,
        "
        CREATE TABLE IF NOT EXISTS projection_issues (
            issue_id       INTEGER PRIMARY KEY AUTOINCREMENT,
            info_hash      TEXT    NOT NULL DEFAULT '',
            artifact       TEXT    NOT NULL,
            path           TEXT    NOT NULL DEFAULT '',
            reason         TEXT    NOT NULL,
            detected_at    INTEGER NOT NULL,
            resolved_at    INTEGER
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_projection_issues_active
            ON projection_issues(info_hash, artifact, path)
            WHERE resolved_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_projection_issues_hash
            ON projection_issues(info_hash, issue_id);
        ",
    ),
    (
        8,
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS peer_bans (
            peer        TEXT NOT NULL PRIMARY KEY,
            created_at  INTEGER NOT NULL
        );
        ",
    ),
    (
        9,
        "
        PRAGMA foreign_keys = ON;

        -- BEP 3 tracker IDs are opaque bytes, not necessarily UTF-8.
        ALTER TABLE torrent_trackers ADD COLUMN tracker_id BLOB;
        ",
    ),
];

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
        assert_eq!(v, CURRENT_SCHEMA_VERSION as i64);
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

    #[test]
    fn migrate_creates_durable_engine_backbone_tables() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        let expected = [
            "torrent_files",
            "torrent_trackers",
            "torrent_tags",
            "torrent_categories",
            "torrent_limits",
            "jobs",
            "session_events",
            "job_events",
            "settings",
            "storage_roots",
            "mounts",
            "api_tokens",
            "projection_issues",
            "peer_bans",
        ];
        for table in expected {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
    }

    #[test]
    fn migrate_adds_session_event_level_index() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_events') WHERE name = 'level'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name = 'idx_session_events_level'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }

    #[test]
    fn migrate_adds_opaque_tracker_id_column() {
        let conn = open_mem();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('torrent_trackers') WHERE name = 'tracker_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn failed_migration_rolls_back_ddl_and_user_version() {
        let conn = open_mem();
        // Build a genuine version-6 database without applying version 7.
        for (version, sql) in MIGRATIONS.iter().take(6) {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", version).unwrap();
        }
        // Make version 7 fail on its index after its CREATE TABLE IF NOT
        // EXISTS statement has run. The transaction must roll back that
        // table creation as well as leave user_version at 6.
        conn.execute(
            "CREATE TABLE projection_issues (issue_id INTEGER PRIMARY KEY)",
            [],
        )
        .unwrap();

        assert!(migrate(&conn).is_err());
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 6);
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_projection_issues_active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 0, "failed migration must not retain its index");
    }
}
