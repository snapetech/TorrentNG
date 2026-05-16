use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TorrentFileRow {
    pub info_hash: String,
    pub file_index: i64,
    pub path: String,
    pub length: i64,
    pub offset: i64,
    pub priority: i64,
    pub wanted: bool,
    pub completed_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TorrentTrackerRow {
    pub info_hash: String,
    pub tracker_index: i64,
    pub tier: i64,
    pub url: String,
    pub status: String,
    pub last_announce_at: Option<i64>,
    pub next_announce_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub failure_reason: Option<String>,
    pub warning_message: Option<String>,
    pub seeders: Option<i64>,
    pub leechers: Option<i64>,
    pub completed: Option<i64>,
    pub uploaded: i64,
    pub downloaded: i64,
    pub left_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TorrentLimitRow {
    pub info_hash: String,
    pub download_limit: Option<i64>,
    pub upload_limit: Option<i64>,
    pub max_connections: Option<i64>,
    pub seed_ratio_limit: Option<f64>,
    pub seed_idle_limit: Option<i64>,
    pub sequential_download: bool,
    pub first_last_piece_prio: bool,
    pub force_start: bool,
    pub super_seeding: bool,
}

impl TorrentFileRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(TorrentFileRow {
            info_hash: row.get(0)?,
            file_index: row.get(1)?,
            path: row.get(2)?,
            length: row.get(3)?,
            offset: row.get(4)?,
            priority: row.get(5)?,
            wanted: row.get::<_, i64>(6)? != 0,
            completed_bytes: row.get(7)?,
        })
    }
}

impl TorrentTrackerRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(TorrentTrackerRow {
            info_hash: row.get(0)?,
            tracker_index: row.get(1)?,
            tier: row.get(2)?,
            url: row.get(3)?,
            status: row.get(4)?,
            last_announce_at: row.get(5)?,
            next_announce_at: row.get(6)?,
            last_success_at: row.get(7)?,
            failure_reason: row.get(8)?,
            warning_message: row.get(9)?,
            seeders: row.get(10)?,
            leechers: row.get(11)?,
            completed: row.get(12)?,
            uploaded: row.get(13)?,
            downloaded: row.get(14)?,
            left_bytes: row.get(15)?,
        })
    }
}

impl TorrentLimitRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(TorrentLimitRow {
            info_hash: row.get(0)?,
            download_limit: row.get(1)?,
            upload_limit: row.get(2)?,
            max_connections: row.get(3)?,
            seed_ratio_limit: row.get(4)?,
            seed_idle_limit: row.get(5)?,
            sequential_download: row.get::<_, i64>(6)? != 0,
            first_last_piece_prio: row.get::<_, i64>(7)? != 0,
            force_start: row.get::<_, i64>(8)? != 0,
            super_seeding: row.get::<_, i64>(9)? != 0,
        })
    }
}

pub fn replace_torrent_files(
    conn: &mut Connection,
    info_hash: &str,
    files: &[TorrentFileRow],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM torrent_files WHERE info_hash = ?1",
        params![info_hash],
    )?;
    for file in files {
        tx.execute(
            "INSERT INTO torrent_files
                (info_hash, file_index, path, length, offset, priority, wanted, completed_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                info_hash,
                file.file_index,
                file.path,
                file.length,
                file.offset,
                file.priority,
                file.wanted as i64,
                file.completed_bytes,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn list_torrent_files(
    conn: &Connection,
    info_hash: &str,
) -> Result<Vec<TorrentFileRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, file_index, path, length, offset, priority, wanted, completed_bytes
         FROM torrent_files
         WHERE info_hash = ?1
         ORDER BY file_index ASC",
    )?;
    let rows = stmt
        .query_map(params![info_hash], TorrentFileRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn replace_torrent_trackers(
    conn: &mut Connection,
    info_hash: &str,
    trackers: &[TorrentTrackerRow],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM torrent_trackers WHERE info_hash = ?1",
        params![info_hash],
    )?;
    for tracker in trackers {
        tx.execute(
            "INSERT INTO torrent_trackers
                (info_hash, tracker_index, tier, url, status, last_announce_at,
                 next_announce_at, last_success_at, failure_reason, warning_message,
                 seeders, leechers, completed, uploaded, downloaded, left_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                info_hash,
                tracker.tracker_index,
                tracker.tier,
                tracker.url,
                tracker.status,
                tracker.last_announce_at,
                tracker.next_announce_at,
                tracker.last_success_at,
                tracker.failure_reason,
                tracker.warning_message,
                tracker.seeders,
                tracker.leechers,
                tracker.completed,
                tracker.uploaded,
                tracker.downloaded,
                tracker.left_bytes,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn list_torrent_trackers(
    conn: &Connection,
    info_hash: &str,
) -> Result<Vec<TorrentTrackerRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, tracker_index, tier, url, status, last_announce_at,
                next_announce_at, last_success_at, failure_reason, warning_message,
                seeders, leechers, completed, uploaded, downloaded, left_bytes
         FROM torrent_trackers
         WHERE info_hash = ?1
         ORDER BY tier ASC, tracker_index ASC",
    )?;
    let rows = stmt
        .query_map(params![info_hash], TorrentTrackerRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn upsert_torrent_limits(conn: &Connection, limits: &TorrentLimitRow) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO torrent_limits
            (info_hash, download_limit, upload_limit, max_connections, seed_ratio_limit,
             seed_idle_limit, sequential_download, first_last_piece_prio, force_start, super_seeding)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(info_hash) DO UPDATE SET
            download_limit=excluded.download_limit,
            upload_limit=excluded.upload_limit,
            max_connections=excluded.max_connections,
            seed_ratio_limit=excluded.seed_ratio_limit,
            seed_idle_limit=excluded.seed_idle_limit,
            sequential_download=excluded.sequential_download,
            first_last_piece_prio=excluded.first_last_piece_prio,
            force_start=excluded.force_start,
            super_seeding=excluded.super_seeding",
        params![
            limits.info_hash,
            limits.download_limit,
            limits.upload_limit,
            limits.max_connections,
            limits.seed_ratio_limit,
            limits.seed_idle_limit,
            limits.sequential_download as i64,
            limits.first_last_piece_prio as i64,
            limits.force_start as i64,
            limits.super_seeding as i64,
        ],
    )?;
    Ok(())
}

pub fn get_torrent_limits(conn: &Connection, info_hash: &str) -> Result<TorrentLimitRow, DbError> {
    conn.query_row(
        "SELECT info_hash, download_limit, upload_limit, max_connections, seed_ratio_limit,
                seed_idle_limit, sequential_download, first_last_piece_prio, force_start, super_seeding
         FROM torrent_limits WHERE info_hash = ?1",
        params![info_hash],
        TorrentLimitRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{schema::migrate, torrent_row};
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let row = torrent_row::TorrentRow {
            info_hash: "a".repeat(40),
            name: "alpha".into(),
            total_length: 100,
            piece_length: 10,
            piece_count: 10,
            is_private: false,
            save_path: "/data".into(),
            category: None,
            tags: Vec::new(),
            state: "stopped".into(),
            added_at: 10,
            completed_at: None,
            uploaded: 0,
            downloaded: 0,
            ratio: 0.0,
            trackers: Vec::new(),
        };
        torrent_row::upsert(&conn, &row).unwrap();
        conn
    }

    #[test]
    fn replace_and_list_files() {
        let mut conn = setup();
        replace_torrent_files(
            &mut conn,
            &"a".repeat(40),
            &[TorrentFileRow {
                info_hash: "a".repeat(40),
                file_index: 0,
                path: "alpha.bin".into(),
                length: 100,
                offset: 0,
                priority: 1,
                wanted: true,
                completed_bytes: 50,
            }],
        )
        .unwrap();
        let files = list_torrent_files(&conn, &"a".repeat(40)).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].completed_bytes, 50);
    }

    #[test]
    fn replace_and_list_trackers() {
        let mut conn = setup();
        replace_torrent_trackers(
            &mut conn,
            &"a".repeat(40),
            &[TorrentTrackerRow {
                info_hash: "a".repeat(40),
                tracker_index: 0,
                tier: 0,
                url: "http://tracker/announce".into(),
                status: "working".into(),
                last_announce_at: Some(20),
                next_announce_at: Some(200),
                last_success_at: Some(20),
                failure_reason: None,
                warning_message: None,
                seeders: Some(1),
                leechers: Some(2),
                completed: Some(3),
                uploaded: 4,
                downloaded: 5,
                left_bytes: 6,
            }],
        )
        .unwrap();
        let trackers = list_torrent_trackers(&conn, &"a".repeat(40)).unwrap();
        assert_eq!(trackers.len(), 1);
        assert_eq!(trackers[0].left_bytes, 6);
    }

    #[test]
    fn upsert_and_get_limits() {
        let conn = setup();
        upsert_torrent_limits(
            &conn,
            &TorrentLimitRow {
                info_hash: "a".repeat(40),
                download_limit: Some(10),
                upload_limit: Some(20),
                max_connections: Some(30),
                seed_ratio_limit: Some(2.0),
                seed_idle_limit: Some(60),
                sequential_download: true,
                first_last_piece_prio: true,
                force_start: false,
                super_seeding: false,
            },
        )
        .unwrap();
        let limits = get_torrent_limits(&conn, &"a".repeat(40)).unwrap();
        assert_eq!(limits.upload_limit, Some(20));
        assert!(limits.sequential_download);
    }
}
