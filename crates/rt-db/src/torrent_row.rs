/// Torrent record stored in the `torrents` table.
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TorrentRow {
    /// Hex-encoded 40-char SHA-1 infohash.
    pub info_hash: String,
    pub name: String,
    pub total_length: i64,
    pub piece_length: i64,
    pub piece_count: i64,
    pub is_private: bool,
    pub save_path: String,
    pub category: Option<String>,
    /// JSON-serialised `Vec<String>`.
    pub tags: Vec<String>,
    pub state: String,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    pub uploaded: i64,
    pub downloaded: i64,
    pub ratio: f64,
    /// JSON-serialised `Vec<String>`.
    pub trackers: Vec<String>,
}

impl TorrentRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let tags_json: String = row.get(8)?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let trackers_json: String = row.get(15)?;
        let trackers: Vec<String> = serde_json::from_str(&trackers_json).unwrap_or_default();
        Ok(TorrentRow {
            info_hash: row.get(0)?,
            name: row.get(1)?,
            total_length: row.get(2)?,
            piece_length: row.get(3)?,
            piece_count: row.get(4)?,
            is_private: row.get::<_, i64>(5)? != 0,
            save_path: row.get(6)?,
            category: row.get(7)?,
            tags,
            state: row.get(9)?,
            added_at: row.get(10)?,
            completed_at: row.get(11)?,
            uploaded: row.get(12)?,
            downloaded: row.get(13)?,
            ratio: row.get(14)?,
            trackers,
        })
    }
}

pub fn upsert(conn: &Connection, row: &TorrentRow) -> Result<(), DbError> {
    let tags_json = serde_json::to_string(&row.tags)?;
    let trackers_json = serde_json::to_string(&row.trackers)?;
    conn.execute(
        "INSERT INTO torrents
            (info_hash, name, total_length, piece_length, piece_count, is_private,
             save_path, category, tags, state, added_at, completed_at,
             uploaded, downloaded, ratio, trackers)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(info_hash) DO UPDATE SET
             name=excluded.name,
             total_length=excluded.total_length,
             piece_length=excluded.piece_length,
             piece_count=excluded.piece_count,
             is_private=excluded.is_private,
             save_path=excluded.save_path,
             state=excluded.state,
             uploaded=excluded.uploaded, downloaded=excluded.downloaded,
             ratio=excluded.ratio, completed_at=excluded.completed_at,
             category=excluded.category, tags=excluded.tags,
             trackers=excluded.trackers",
        params![
            row.info_hash,
            row.name,
            row.total_length,
            row.piece_length,
            row.piece_count,
            row.is_private as i64,
            row.save_path,
            row.category,
            tags_json,
            row.state,
            row.added_at,
            row.completed_at,
            row.uploaded,
            row.downloaded,
            row.ratio,
            trackers_json,
        ],
    )?;
    persist_normalized_labels(conn, row)?;
    Ok(())
}

pub fn upsert_in_tx(tx: &rusqlite::Transaction<'_>, row: &TorrentRow) -> Result<(), DbError> {
    let tags_json = serde_json::to_string(&row.tags)?;
    let trackers_json = serde_json::to_string(&row.trackers)?;
    tx.execute(
        "INSERT INTO torrents
            (info_hash, name, total_length, piece_length, piece_count, is_private,
             save_path, category, tags, state, added_at, completed_at,
             uploaded, downloaded, ratio, trackers)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(info_hash) DO UPDATE SET
             name=excluded.name,
             total_length=excluded.total_length,
             piece_length=excluded.piece_length,
             piece_count=excluded.piece_count,
             is_private=excluded.is_private,
             save_path=excluded.save_path,
             state=excluded.state,
             uploaded=excluded.uploaded, downloaded=excluded.downloaded,
             ratio=excluded.ratio, completed_at=excluded.completed_at,
             category=excluded.category, tags=excluded.tags,
             trackers=excluded.trackers",
        params![
            row.info_hash,
            row.name,
            row.total_length,
            row.piece_length,
            row.piece_count,
            row.is_private as i64,
            row.save_path,
            row.category,
            tags_json,
            row.state,
            row.added_at,
            row.completed_at,
            row.uploaded,
            row.downloaded,
            row.ratio,
            trackers_json,
        ],
    )?;
    persist_normalized_labels_in_tx(tx, row)?;
    Ok(())
}

fn persist_normalized_labels(conn: &Connection, row: &TorrentRow) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM torrent_tags WHERE info_hash = ?1",
        params![row.info_hash],
    )?;
    for tag in &row.tags {
        conn.execute(
            "INSERT OR IGNORE INTO torrent_tags (info_hash, tag) VALUES (?1, ?2)",
            params![row.info_hash, tag],
        )?;
    }
    if let Some(category) = &row.category {
        conn.execute(
            "INSERT OR IGNORE INTO torrent_categories (name, save_path, created_at)
             VALUES (?1, NULL, ?2)",
            params![category, row.added_at],
        )?;
    }
    Ok(())
}

fn persist_normalized_labels_in_tx(
    tx: &rusqlite::Transaction<'_>,
    row: &TorrentRow,
) -> Result<(), DbError> {
    tx.execute(
        "DELETE FROM torrent_tags WHERE info_hash = ?1",
        params![row.info_hash],
    )?;
    for tag in &row.tags {
        tx.execute(
            "INSERT OR IGNORE INTO torrent_tags (info_hash, tag) VALUES (?1, ?2)",
            params![row.info_hash, tag],
        )?;
    }
    if let Some(category) = &row.category {
        tx.execute(
            "INSERT OR IGNORE INTO torrent_categories (name, save_path, created_at)
             VALUES (?1, NULL, ?2)",
            params![category, row.added_at],
        )?;
    }
    Ok(())
}

pub fn list_torrent_tags(conn: &Connection, info_hash: &str) -> Result<Vec<String>, DbError> {
    let mut stmt =
        conn.prepare("SELECT tag FROM torrent_tags WHERE info_hash = ?1 ORDER BY tag ASC")?;
    let tags = stmt
        .query_map(params![info_hash], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(tags)
}

pub fn list_categories(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare("SELECT name FROM torrent_categories ORDER BY name ASC")?;
    let categories = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(categories)
}

pub fn get(conn: &Connection, info_hash: &str) -> Result<TorrentRow, DbError> {
    conn.query_row(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, ratio, trackers
         FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        TorrentRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

pub fn delete(conn: &Connection, info_hash: &str) -> Result<bool, DbError> {
    let n = conn.execute(
        "DELETE FROM torrents WHERE info_hash = ?1",
        params![info_hash],
    )?;
    Ok(n > 0)
}

pub fn list_all(conn: &Connection) -> Result<Vec<TorrentRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, ratio, trackers
         FROM torrents ORDER BY added_at DESC",
    )?;
    let rows = stmt
        .query_map([], TorrentRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_by_state(conn: &Connection, state: &str) -> Result<Vec<TorrentRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, ratio, trackers
         FROM torrents WHERE state = ?1 ORDER BY added_at DESC",
    )?;
    let rows = stmt
        .query_map(params![state], TorrentRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn sample() -> TorrentRow {
        TorrentRow {
            info_hash: "a".repeat(40),
            name: "test.torrent".into(),
            total_length: 1_000_000,
            piece_length: 262144,
            piece_count: 4,
            is_private: true,
            save_path: "/data".into(),
            category: Some("movies".into()),
            tags: vec!["hd".into(), "bluray".into()],
            state: "seeding".into(),
            added_at: 1_700_000_000,
            completed_at: Some(1_700_001_000),
            uploaded: 5_000_000,
            downloaded: 1_000_000,
            ratio: 5.0,
            trackers: vec!["http://tracker.example.com/announce".into()],
        }
    }

    #[test]
    fn upsert_and_get() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.name, row.name);
        assert_eq!(fetched.tags, row.tags);
        assert_eq!(fetched.trackers, row.trackers);
        assert!(fetched.is_private);
    }

    #[test]
    fn upsert_persists_normalized_labels() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();

        assert_eq!(
            list_torrent_tags(&conn, &row.info_hash).unwrap(),
            vec!["bluray".to_owned(), "hd".to_owned()]
        );
        assert_eq!(list_categories(&conn).unwrap(), vec!["movies".to_owned()]);
    }

    #[test]
    fn upsert_replaces_normalized_tags() {
        let conn = setup();
        let mut row = sample();
        upsert(&conn, &row).unwrap();
        row.tags = vec!["archive".into()];
        upsert(&conn, &row).unwrap();

        assert_eq!(
            list_torrent_tags(&conn, &row.info_hash).unwrap(),
            vec!["archive".to_owned()]
        );
    }

    #[test]
    fn upsert_updates_state() {
        let conn = setup();
        let mut row = sample();
        upsert(&conn, &row).unwrap();
        row.state = "paused".into();
        upsert(&conn, &row).unwrap();
        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.state, "paused");
    }

    #[test]
    fn get_not_found_errors() {
        let conn = setup();
        let err = get(&conn, "nonexistent").unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }

    #[test]
    fn delete_removes_record() {
        let conn = setup();
        upsert(&conn, &sample()).unwrap();
        let removed = delete(&conn, &sample().info_hash).unwrap();
        assert!(removed);
        assert!(get(&conn, &sample().info_hash).is_err());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let conn = setup();
        assert!(!delete(&conn, "nope").unwrap());
    }

    #[test]
    fn list_all_returns_all() {
        let conn = setup();
        let mut r1 = sample();
        let mut r2 = sample();
        r2.info_hash = "b".repeat(40);
        r2.name = "other.torrent".into();
        upsert(&conn, &r1).unwrap();
        upsert(&conn, &r2).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 2);
        // Update r1 state
        r1.state = "paused".into();
        upsert(&conn, &r1).unwrap();
        let paused = list_by_state(&conn, "paused").unwrap();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].info_hash, r1.info_hash);
    }
}
