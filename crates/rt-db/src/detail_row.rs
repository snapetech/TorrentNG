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
    /// Opaque BEP 3 tracker ID. It is a BLOB because trackers are not
    /// required to return UTF-8.
    #[serde(default)]
    pub tracker_id: Option<Vec<u8>>,
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

/// Compact tracker status aggregate used by engine stats. It avoids
/// materializing every tracker row just to count a few statuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TorrentTrackerStatusCounts {
    pub total: u64,
    pub working: u64,
    pub warning: u64,
    pub error: u64,
}

/// One row in the native tracker-health aggregate. A torrent is counted once
/// per tracker URL even if its metainfo contains the same URL more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentTrackerHealthRow {
    pub tracker: String,
    pub torrent_count: u64,
    pub active_count: u64,
    pub complete_count: u64,
    pub error_count: u64,
    pub seed_count: u64,
    /// Tracker-reported leechers, not currently connected sessions.
    pub peer_count: u64,
    pub last_updated: Option<i64>,
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
    pub sequential_download_from_piece: Option<i64>,
    pub first_last_piece_prio: bool,
    pub force_start: bool,
    pub super_seeding: bool,
    pub auto_tmm: bool,
    pub auto_management: bool,
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
            tracker_id: row.get(4)?,
            status: row.get(5)?,
            last_announce_at: row.get(6)?,
            next_announce_at: row.get(7)?,
            last_success_at: row.get(8)?,
            failure_reason: row.get(9)?,
            warning_message: row.get(10)?,
            seeders: row.get(11)?,
            leechers: row.get(12)?,
            completed: row.get(13)?,
            uploaded: row.get(14)?,
            downloaded: row.get(15)?,
            left_bytes: row.get(16)?,
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
            sequential_download_from_piece: row.get(7)?,
            first_last_piece_prio: row.get::<_, i64>(8)? != 0,
            force_start: row.get::<_, i64>(9)? != 0,
            super_seeding: row.get::<_, i64>(10)? != 0,
            auto_tmm: row.get::<_, i64>(11)? != 0,
            auto_management: row.get::<_, i64>(12)? != 0,
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

pub fn replace_torrent_files_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    files: &[TorrentFileRow],
) -> Result<(), DbError> {
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

/// Count a torrent's durable file projection without materializing every file
/// row. Startup reconciliation uses this to identify interrupted metadata
/// commits while keeping dormant restore bounded.
pub fn count_torrent_files(conn: &Connection, info_hash: &str) -> Result<u64, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM torrent_files WHERE info_hash = ?1",
        params![info_hash],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
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
                (info_hash, tracker_index, tier, url, tracker_id, status, last_announce_at,
                 next_announce_at, last_success_at, failure_reason, warning_message,
                 seeders, leechers, completed, uploaded, downloaded, left_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                info_hash,
                tracker.tracker_index,
                tracker.tier,
                tracker.url,
                tracker.tracker_id,
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

pub fn replace_torrent_trackers_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    trackers: &[TorrentTrackerRow],
) -> Result<(), DbError> {
    tx.execute(
        "DELETE FROM torrent_trackers WHERE info_hash = ?1",
        params![info_hash],
    )?;
    for tracker in trackers {
        tx.execute(
            "INSERT INTO torrent_trackers
                (info_hash, tracker_index, tier, url, tracker_id, status, last_announce_at,
                 next_announce_at, last_success_at, failure_reason, warning_message,
                 seeders, leechers, completed, uploaded, downloaded, left_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                info_hash,
                tracker.tracker_index,
                tracker.tier,
                tracker.url,
                tracker.tracker_id,
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
    Ok(())
}

pub fn list_torrent_trackers(
    conn: &Connection,
    info_hash: &str,
) -> Result<Vec<TorrentTrackerRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, tracker_index, tier, url, tracker_id, status, last_announce_at,
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

pub fn list_all_torrent_trackers(conn: &Connection) -> Result<Vec<TorrentTrackerRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, tracker_index, tier, url, tracker_id, status, last_announce_at,
                next_announce_at, last_success_at, failure_reason, warning_message,
                seeders, leechers, completed, uploaded, downloaded, left_bytes
         FROM torrent_trackers
         ORDER BY info_hash ASC, tier ASC, tracker_index ASC",
    )?;
    let rows = stmt
        .query_map([], TorrentTrackerRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Return torrent hashes whose normalized tracker URL contains `needle`.
/// `instr` is used instead of `LIKE` so tracker text cannot introduce
/// wildcard semantics.  This is the database-side half of native automation
/// tracker matching; callers can intersect the result with their live
/// registry projection before applying an action.
pub fn list_torrent_hashes_by_tracker(
    conn: &Connection,
    needle: &str,
) -> Result<Vec<String>, DbError> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT DISTINCT info_hash
         FROM torrent_trackers
         WHERE instr(url, ?1) > 0
         ORDER BY info_hash ASC",
    )?;
    let rows = stmt
        .query_map(params![needle], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(rows)
}

pub fn torrent_tracker_status_counts(
    conn: &Connection,
) -> Result<TorrentTrackerStatusCounts, DbError> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*)
         FROM torrent_trackers
         GROUP BY status",
    )?;
    let mut counts = TorrentTrackerStatusCounts::default();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (status, count) = row?;
        let count = count.max(0) as u64;
        counts.total = counts.total.saturating_add(count);
        match status.as_str() {
            "working" => counts.working = counts.working.saturating_add(count),
            "warning" => counts.warning = counts.warning.saturating_add(count),
            "error" => counts.error = counts.error.saturating_add(count),
            _ => {}
        }
    }
    Ok(counts)
}

/// Aggregate normalized tracker state without materializing every tracker
/// row in the API process. The inner query collapses duplicate tracker URLs
/// within a torrent before the outer query sums peer counts.
pub fn torrent_tracker_health(conn: &Connection) -> Result<Vec<TorrentTrackerHealthRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT tracker,
                COUNT(*) AS torrent_count,
                COALESCE(SUM(active_count), 0) AS active_count,
                COALESCE(SUM(complete_count), 0) AS complete_count,
                COALESCE(SUM(error_count), 0) AS error_count,
                COALESCE(SUM(seed_count), 0) AS seed_count,
                COALESCE(SUM(peer_count), 0) AS peer_count,
                MAX(last_updated) AS last_updated
         FROM (
             SELECT tt.url AS tracker,
                    tt.info_hash,
                    CASE WHEN t.state IN ('downloading', 'seeding')
                         THEN 1 ELSE 0 END AS active_count,
                    CASE WHEN t.completed_at IS NOT NULL OR t.state = 'seeding'
                         THEN 1 ELSE 0 END AS complete_count,
                    MAX(CASE WHEN tt.status = 'error' THEN 1 ELSE 0 END)
                        AS error_count,
                    MAX(COALESCE(tt.seeders, 0)) AS seed_count,
                    MAX(COALESCE(tt.leechers, 0)) AS peer_count,
                    MAX(COALESCE(tt.last_announce_at, tt.last_success_at))
                        AS last_updated
             FROM torrent_trackers AS tt
             INNER JOIN torrents AS t ON t.info_hash = tt.info_hash
             WHERE tt.url <> ''
             GROUP BY tt.url, tt.info_hash
         )
         GROUP BY tracker
         ORDER BY error_count DESC, torrent_count DESC, tracker COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let to_u64 = |value: i64| u64::try_from(value.max(0)).unwrap_or(0);
            Ok(TorrentTrackerHealthRow {
                tracker: row.get(0)?,
                torrent_count: to_u64(row.get(1)?),
                active_count: to_u64(row.get(2)?),
                complete_count: to_u64(row.get(3)?),
                error_count: to_u64(row.get(4)?),
                seed_count: to_u64(row.get(5)?),
                peer_count: to_u64(row.get(6)?),
                last_updated: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn upsert_torrent_limits(conn: &Connection, limits: &TorrentLimitRow) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO torrent_limits
            (info_hash, download_limit, upload_limit, max_connections, seed_ratio_limit,
             seed_idle_limit, sequential_download, sequential_download_from_piece,
             first_last_piece_prio, force_start, super_seeding, auto_tmm, auto_management)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(info_hash) DO UPDATE SET
            download_limit=excluded.download_limit,
            upload_limit=excluded.upload_limit,
            max_connections=excluded.max_connections,
            seed_ratio_limit=excluded.seed_ratio_limit,
            seed_idle_limit=excluded.seed_idle_limit,
            sequential_download=excluded.sequential_download,
            sequential_download_from_piece=excluded.sequential_download_from_piece,
            first_last_piece_prio=excluded.first_last_piece_prio,
            force_start=excluded.force_start,
            super_seeding=excluded.super_seeding,
            auto_tmm=excluded.auto_tmm,
            auto_management=excluded.auto_management",
        params![
            limits.info_hash,
            limits.download_limit,
            limits.upload_limit,
            limits.max_connections,
            limits.seed_ratio_limit,
            limits.seed_idle_limit,
            limits.sequential_download as i64,
            limits.sequential_download_from_piece,
            limits.first_last_piece_prio as i64,
            limits.force_start as i64,
            limits.super_seeding as i64,
            limits.auto_tmm as i64,
            limits.auto_management as i64,
        ],
    )?;
    Ok(())
}

/// Upsert torrent limits inside a caller-owned transaction so an event or
/// another projection row can commit with the limit change.
pub fn upsert_torrent_limits_in_tx(
    tx: &rusqlite::Transaction<'_>,
    limits: &TorrentLimitRow,
) -> Result<(), DbError> {
    tx.execute(
        "INSERT INTO torrent_limits
            (info_hash, download_limit, upload_limit, max_connections, seed_ratio_limit,
             seed_idle_limit, sequential_download, sequential_download_from_piece,
             first_last_piece_prio, force_start, super_seeding, auto_tmm, auto_management)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(info_hash) DO UPDATE SET
            download_limit=excluded.download_limit,
            upload_limit=excluded.upload_limit,
            max_connections=excluded.max_connections,
            seed_ratio_limit=excluded.seed_ratio_limit,
            seed_idle_limit=excluded.seed_idle_limit,
            sequential_download=excluded.sequential_download,
            sequential_download_from_piece=excluded.sequential_download_from_piece,
            first_last_piece_prio=excluded.first_last_piece_prio,
            force_start=excluded.force_start,
            super_seeding=excluded.super_seeding,
            auto_tmm=excluded.auto_tmm,
            auto_management=excluded.auto_management",
        params![
            limits.info_hash,
            limits.download_limit,
            limits.upload_limit,
            limits.max_connections,
            limits.seed_ratio_limit,
            limits.seed_idle_limit,
            limits.sequential_download as i64,
            limits.sequential_download_from_piece,
            limits.first_last_piece_prio as i64,
            limits.force_start as i64,
            limits.super_seeding as i64,
            limits.auto_tmm as i64,
            limits.auto_management as i64,
        ],
    )?;
    Ok(())
}

pub fn get_torrent_limits(conn: &Connection, info_hash: &str) -> Result<TorrentLimitRow, DbError> {
    conn.query_row(
        "SELECT info_hash, download_limit, upload_limit, max_connections, seed_ratio_limit,
                seed_idle_limit, sequential_download, sequential_download_from_piece,
                first_last_piece_prio, force_start, super_seeding, auto_tmm, auto_management
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
                tracker_id: Some(vec![0x00, 0xff, 0x80]),
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
        assert_eq!(trackers[0].seeders, Some(1));
        assert_eq!(trackers[0].leechers, Some(2));
        assert_eq!(trackers[0].completed, Some(3));
        assert_eq!(trackers[0].uploaded, 4);
        assert_eq!(trackers[0].downloaded, 5);
        assert_eq!(trackers[0].left_bytes, 6);
        assert_eq!(trackers[0].tracker_id, Some(vec![0x00, 0xff, 0x80]));
        let all = list_all_torrent_trackers(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, "working");
        assert_eq!(
            list_torrent_hashes_by_tracker(&conn, "tracker/announce").unwrap(),
            vec!["a".repeat(40)]
        );
        assert!(list_torrent_hashes_by_tracker(&conn, "[%]")
            .unwrap()
            .is_empty());
        assert_eq!(
            torrent_tracker_status_counts(&conn).unwrap(),
            TorrentTrackerStatusCounts {
                total: 1,
                working: 1,
                warning: 0,
                error: 0,
            }
        );
    }

    #[test]
    fn tracker_health_groups_urls_and_deduplicates_per_torrent() {
        let mut conn = setup();
        let first_hash = "a".repeat(40);
        let mut first = torrent_row::get(&conn, &first_hash).unwrap();
        first.state = "seeding".into();
        first.completed_at = Some(30);
        torrent_row::upsert(&conn, &first).unwrap();

        let second_hash = "b".repeat(40);
        let mut second = first.clone();
        second.info_hash = second_hash.clone();
        second.name = "beta".into();
        second.state = "downloading".into();
        second.completed_at = None;
        torrent_row::upsert(&conn, &second).unwrap();

        let tracker = |info_hash: &str,
                       tracker_index: i64,
                       url: &str,
                       status: &str,
                       seeders: i64,
                       leechers: i64,
                       last_announce_at: i64| TorrentTrackerRow {
            info_hash: info_hash.into(),
            tracker_index,
            tier: 0,
            url: url.into(),
            tracker_id: None,
            status: status.into(),
            last_announce_at: Some(last_announce_at),
            next_announce_at: None,
            last_success_at: None,
            failure_reason: None,
            warning_message: None,
            seeders: Some(seeders),
            leechers: Some(leechers),
            completed: None,
            uploaded: 0,
            downloaded: 0,
            left_bytes: 0,
        };
        let url = "https://tracker.example/announce";
        replace_torrent_trackers(
            &mut conn,
            &first_hash,
            &[
                tracker(&first_hash, 0, url, "working", 10, 4, 20),
                tracker(&first_hash, 1, url, "error", 12, 5, 50),
            ],
        )
        .unwrap();
        replace_torrent_trackers(
            &mut conn,
            &second_hash,
            &[tracker(&second_hash, 0, url, "working", 2, 3, 40)],
        )
        .unwrap();

        let rows = torrent_tracker_health(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            TorrentTrackerHealthRow {
                tracker: url.into(),
                torrent_count: 2,
                active_count: 2,
                complete_count: 1,
                error_count: 1,
                seed_count: 14,
                peer_count: 8,
                last_updated: Some(50),
            }
        );
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
                sequential_download_from_piece: Some(3),
                first_last_piece_prio: true,
                force_start: false,
                super_seeding: false,
                auto_tmm: true,
                auto_management: true,
            },
        )
        .unwrap();
        let limits = get_torrent_limits(&conn, &"a".repeat(40)).unwrap();
        assert_eq!(limits.upload_limit, Some(20));
        assert!(limits.sequential_download);
        assert_eq!(limits.sequential_download_from_piece, Some(3));
        assert!(limits.auto_tmm);
        assert!(limits.auto_management);
    }
}
