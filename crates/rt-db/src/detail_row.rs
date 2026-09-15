use rusqlite::{
    params,
    types::{Type, ValueRef},
    Connection, OptionalExtension, Row,
};
use serde::{Deserialize, Serialize};
use std::io;

use crate::error::DbError;
use crate::torrent_row::{
    MAX_TORRENT_TRACKER_RESULT_BYTES, MAX_TORRENT_TRACKER_RESULT_ITEMS,
    MAX_TORRENT_TRACKER_URL_BYTES,
};

pub const MAX_TORRENT_TRACKER_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_TORRENT_TRACKER_ID_BYTES: usize = 16 * 1024;
pub const MAX_TORRENT_TRACKER_INFO_HASH_BYTES: usize = 64;
pub const MAX_TORRENT_FILE_PATH_BYTES: usize = 16 * 1024;
pub const MAX_TORRENT_FILE_RESULT_ITEMS: usize = 100_000;
pub const MAX_TORRENT_FILE_RESULT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TORRENT_FILE_INFO_HASH_BYTES: usize = 64;

// Tracker filtering is used to intersect the live registry, so it may need
// to return the full supported session population rather than the smaller
// mutation/list-page limit. Keep that population and its hash strings bounded
// before the API builds a HashSet from it.
const MAX_TRACKER_MATCH_RESULT_ITEMS: usize = 100_000;
const MAX_TRACKER_MATCH_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_GLOBAL_TRACKER_RESULT_ITEMS: usize = 100_000;
const MAX_GLOBAL_TRACKER_RESULT_BYTES: usize = 256 * 1024 * 1024;

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

/// One row in the TorrentNG client tracker-health aggregate. A torrent is counted once
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
            info_hash: bounded_file_text_column(
                row,
                0,
                "torrent file info hash",
                MAX_TORRENT_FILE_INFO_HASH_BYTES,
            )?,
            file_index: row.get(1)?,
            path: bounded_file_text_column(
                row,
                2,
                "torrent file path",
                MAX_TORRENT_FILE_PATH_BYTES,
            )?,
            length: row.get(3)?,
            offset: row.get(4)?,
            priority: row.get(5)?,
            wanted: row.get::<_, i64>(6)? != 0,
            completed_bytes: row.get(7)?,
        })
    }
}

fn file_column_value_error(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn file_text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(file_column_value_error(
            column,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_file_text_column(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = file_text_value(row, column, field)? else {
        return Err(file_column_value_error(column, format!("{field} is NULL")));
    };
    if value.len() > maximum {
        return Err(file_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| file_column_value_error(column, format!("{field} is not UTF-8: {error}")))
}

fn validate_torrent_files(files: &[TorrentFileRow]) -> Result<(), DbError> {
    if files.len() > MAX_TORRENT_FILE_RESULT_ITEMS {
        return Err(DbError::ValueTooLarge {
            field: "torrent file result items",
            len: files.len() as u64,
            max: MAX_TORRENT_FILE_RESULT_ITEMS as u64,
        });
    }
    let mut bytes = 0usize;
    for file in files {
        if file.info_hash.len() > MAX_TORRENT_FILE_INFO_HASH_BYTES {
            return Err(DbError::ValueTooLarge {
                field: "torrent file info hash",
                len: file.info_hash.len() as u64,
                max: MAX_TORRENT_FILE_INFO_HASH_BYTES as u64,
            });
        }
        if file.path.len() > MAX_TORRENT_FILE_PATH_BYTES {
            return Err(DbError::ValueTooLarge {
                field: "torrent file path",
                len: file.path.len() as u64,
                max: MAX_TORRENT_FILE_PATH_BYTES as u64,
            });
        }
        bytes = bytes
            .saturating_add(file.info_hash.len())
            .saturating_add(file.path.len());
    }
    if bytes > MAX_TORRENT_FILE_RESULT_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "torrent file result bytes",
            len: bytes as u64,
            max: MAX_TORRENT_FILE_RESULT_BYTES as u64,
        });
    }
    Ok(())
}

impl TorrentTrackerRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(TorrentTrackerRow {
            info_hash: bounded_tracker_text_column(
                row,
                0,
                "tracker info hash",
                MAX_TORRENT_TRACKER_INFO_HASH_BYTES,
            )?,
            tracker_index: row.get(1)?,
            tier: row.get(2)?,
            url: bounded_tracker_text_column(row, 3, "tracker URL", MAX_TORRENT_TRACKER_URL_BYTES)?,
            tracker_id: optional_bounded_tracker_id_column(row, 4)?,
            status: bounded_tracker_text_column(
                row,
                5,
                "tracker status",
                MAX_TORRENT_TRACKER_TEXT_BYTES,
            )?,
            last_announce_at: row.get(6)?,
            next_announce_at: row.get(7)?,
            last_success_at: row.get(8)?,
            failure_reason: optional_bounded_tracker_text_column(
                row,
                9,
                "tracker failure reason",
                MAX_TORRENT_TRACKER_TEXT_BYTES,
            )?,
            warning_message: optional_bounded_tracker_text_column(
                row,
                10,
                "tracker warning message",
                MAX_TORRENT_TRACKER_TEXT_BYTES,
            )?,
            seeders: row.get(11)?,
            leechers: row.get(12)?,
            completed: row.get(13)?,
            uploaded: row.get(14)?,
            downloaded: row.get(15)?,
            left_bytes: row.get(16)?,
        })
    }
}

fn tracker_column_value_error(
    column: usize,
    value_type: Type,
    message: impl Into<String>,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        value_type,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn tracker_text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(tracker_column_value_error(
            column,
            Type::Text,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_tracker_text_column(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = tracker_text_value(row, column, field)? else {
        return Err(tracker_column_value_error(
            column,
            Type::Text,
            format!("{field} is NULL"),
        ));
    };
    if value.len() > maximum {
        return Err(tracker_column_value_error(
            column,
            Type::Text,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| {
            tracker_column_value_error(column, Type::Text, format!("{field} is not UTF-8: {error}"))
        })
}

fn optional_bounded_tracker_text_column(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<Option<String>> {
    let Some(value) = tracker_text_value(row, column, field)? else {
        return Ok(None);
    };
    if value.len() > maximum {
        return Err(tracker_column_value_error(
            column,
            Type::Text,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(|value| Some(value.to_owned()))
        .map_err(|error| {
            tracker_column_value_error(column, Type::Text, format!("{field} is not UTF-8: {error}"))
        })
}

fn optional_bounded_tracker_id_column(
    row: &Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<Vec<u8>>> {
    let Some(value) = tracker_text_value(row, column, "tracker ID")? else {
        return Ok(None);
    };
    if value.len() > MAX_TORRENT_TRACKER_ID_BYTES {
        return Err(tracker_column_value_error(
            column,
            Type::Blob,
            format!(
                "tracker ID is {} bytes; maximum is {MAX_TORRENT_TRACKER_ID_BYTES}",
                value.len()
            ),
        ));
    }
    Ok(Some(value.to_owned()))
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
    validate_torrent_files(files)?;
    if info_hash.len() > MAX_TORRENT_FILE_INFO_HASH_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "torrent file info hash",
            len: info_hash.len() as u64,
            max: MAX_TORRENT_FILE_INFO_HASH_BYTES as u64,
        });
    }
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
    validate_torrent_files(files)?;
    if info_hash.len() > MAX_TORRENT_FILE_INFO_HASH_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "torrent file info hash",
            len: info_hash.len() as u64,
            max: MAX_TORRENT_FILE_INFO_HASH_BYTES as u64,
        });
    }
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
                , COUNT(*) OVER () AS result_count
                , COALESCE(SUM(
                    length(CAST(info_hash AS BLOB)) + length(CAST(path AS BLOB))
                ) OVER (), 0) AS result_bytes
         FROM torrent_files
         WHERE info_hash = ?1
         ORDER BY file_index ASC",
    )?;
    let mut rows = stmt.query(params![info_hash])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(8)?.max(0) as u64;
    let result_bytes = first.get::<_, i64>(9)?.max(0) as u64;
    if result_count > MAX_TORRENT_FILE_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent file result items",
            len: result_count,
            max: MAX_TORRENT_FILE_RESULT_ITEMS as u64,
        });
    }
    if result_bytes > MAX_TORRENT_FILE_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent file result bytes",
            len: result_bytes,
            max: MAX_TORRENT_FILE_RESULT_BYTES as u64,
        });
    }

    let mut result = Vec::with_capacity(result_count as usize);
    result.push(TorrentFileRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(TorrentFileRow::from_row(row)?);
    }
    Ok(result)
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
    let (count, bytes) = torrent_tracker_snapshot_size(conn, info_hash)?;
    validate_tracker_snapshot_result(count, bytes)?;
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

/// Return only tracker URLs for compatibility mutations that need to edit
/// the current list. Avoid materializing status, counters, messages, and
/// opaque tracker IDs when the caller will immediately replace the URLs.
pub fn list_torrent_tracker_urls(
    conn: &Connection,
    info_hash: &str,
) -> Result<Vec<String>, DbError> {
    let (count, bytes) = torrent_tracker_snapshot_size(conn, info_hash)?;
    validate_tracker_snapshot_result(count, bytes)?;
    let mut stmt = conn.prepare(
        "SELECT url
         FROM torrent_trackers
         WHERE info_hash = ?1
         ORDER BY tier ASC, tracker_index ASC",
    )?;
    let rows = stmt
        .query_map(params![info_hash], |row| {
            bounded_tracker_text_column(row, 0, "tracker URL", MAX_TORRENT_TRACKER_URL_BYTES)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Return only the qBittorrent-compatible primary tracker projection.
///
/// The compatibility list path needs the first tracker URL and total count,
/// not every tracker status, counter, message, and opaque tracker ID. Keep
/// that read narrow so a large tracker set cannot be materialized for each
/// live torrent projection.
pub fn torrent_tracker_projection(
    conn: &Connection,
    info_hash: &str,
) -> Result<Option<(String, u64)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT url,
                (SELECT COUNT(*) FROM torrent_trackers WHERE info_hash = ?1)
         FROM torrent_trackers
         WHERE info_hash = ?1
         ORDER BY tier ASC, tracker_index ASC
         LIMIT 1",
    )?;
    let projection = stmt
        .query_row(params![info_hash], |row| {
            let count: i64 = row.get(1)?;
            let url =
                bounded_tracker_text_column(row, 0, "tracker URL", MAX_TORRENT_TRACKER_URL_BYTES)?;
            Ok((url, count.max(0) as u64))
        })
        .optional()?;
    Ok(projection)
}

/// Return the row count and UTF-8 byte footprint needed before materializing
/// a compatibility tracker snapshot. Keep this aggregate narrow: callers use
/// it for memory admission before loading status text, tracker IDs, and
/// counters for every tracker row.
pub fn torrent_tracker_snapshot_size(
    conn: &Connection,
    info_hash: &str,
) -> Result<(u64, u64), DbError> {
    let (count, bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(
                    length(CAST(info_hash AS BLOB))
                    + length(CAST(url AS BLOB))
                    + length(CAST(status AS BLOB))
                    + COALESCE(length(CAST(failure_reason AS BLOB)), 0)
                    + COALESCE(length(CAST(warning_message AS BLOB)), 0)
                    + COALESCE(length(tracker_id), 0)
                ), 0)
         FROM torrent_trackers
         WHERE info_hash = ?1",
        params![info_hash],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((count.max(0) as u64, bytes.max(0) as u64))
}

fn validate_tracker_snapshot_result(count: u64, bytes: u64) -> Result<(), DbError> {
    if count > MAX_TORRENT_TRACKER_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent tracker result items",
            len: count,
            max: MAX_TORRENT_TRACKER_RESULT_ITEMS as u64,
        });
    }
    if bytes > MAX_TORRENT_TRACKER_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent tracker result bytes",
            len: bytes,
            max: MAX_TORRENT_TRACKER_RESULT_BYTES as u64,
        });
    }
    Ok(())
}

/// Return the earliest persisted announce deadline for one torrent without
/// materializing its tracker rows.
pub fn torrent_tracker_deadline(
    conn: &Connection,
    info_hash: &str,
) -> Result<Option<i64>, DbError> {
    conn.query_row(
        "SELECT MIN(next_announce_at)
         FROM torrent_trackers
         WHERE info_hash = ?1",
        params![info_hash],
        |row| row.get(0),
    )
    .map_err(DbError::from)
}

/// Check whether a torrent has any normalized tracker rows without loading
/// the tracker projection. Startup repair uses this narrow probe while
/// processing legacy rows in bounded batches.
pub fn torrent_tracker_rows_exist(conn: &Connection, info_hash: &str) -> Result<bool, DbError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM torrent_trackers WHERE info_hash = ?1)",
        params![info_hash],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

pub fn list_all_torrent_trackers(conn: &Connection) -> Result<Vec<TorrentTrackerRow>, DbError> {
    let (count, bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(
                    length(CAST(info_hash AS BLOB))
                    + length(CAST(url AS BLOB))
                    + length(CAST(status AS BLOB))
                    + COALESCE(length(CAST(failure_reason AS BLOB)), 0)
                    + COALESCE(length(CAST(warning_message AS BLOB)), 0)
                    + COALESCE(length(tracker_id), 0)
                ), 0)
         FROM torrent_trackers",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let count = count.max(0) as u64;
    let bytes = bytes.max(0) as u64;
    if count > MAX_GLOBAL_TRACKER_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "global torrent tracker result items",
            len: count,
            max: MAX_GLOBAL_TRACKER_RESULT_ITEMS as u64,
        });
    }
    if bytes > MAX_GLOBAL_TRACKER_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "global torrent tracker result bytes",
            len: bytes,
            max: MAX_GLOBAL_TRACKER_RESULT_BYTES as u64,
        });
    }
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

/// Return the earliest persisted announce deadline for each torrent.
///
/// Startup only needs this aggregate to decide which dormant torrents must be
/// promoted. Keep the query narrow so restoring a large database does not
/// materialize tracker URLs, status text, counters, and opaque tracker IDs.
pub fn list_torrent_tracker_deadlines(conn: &Connection) -> Result<Vec<(String, i64)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, MIN(next_announce_at)
         FROM torrent_trackers
         WHERE next_announce_at IS NOT NULL
         GROUP BY info_hash
         ORDER BY info_hash ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        if result.len() >= MAX_TRACKER_MATCH_RESULT_ITEMS {
            return Err(DbError::ValueTooLarge {
                field: "tracker deadline result items",
                len: (result.len() + 1) as u64,
                max: MAX_TRACKER_MATCH_RESULT_ITEMS as u64,
            });
        }
        let info_hash = bounded_tracker_text_column(
            row,
            0,
            "tracker deadline info hash",
            MAX_TORRENT_TRACKER_INFO_HASH_BYTES,
        )?;
        result.push((info_hash, row.get(1)?));
    }
    let bytes = result
        .iter()
        .map(|(info_hash, _)| info_hash.len())
        .sum::<usize>();
    if bytes > MAX_TRACKER_MATCH_RESULT_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "tracker deadline result bytes",
            len: bytes as u64,
            max: MAX_TRACKER_MATCH_RESULT_BYTES as u64,
        });
    }
    Ok(result)
}

/// Return the set of torrents that have persisted tracker rows without
/// materializing each tracker detail row.
pub fn list_torrent_tracker_hashes(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT info_hash
         FROM torrent_trackers
         ORDER BY info_hash ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        if result.len() >= MAX_TRACKER_MATCH_RESULT_ITEMS {
            return Err(DbError::ValueTooLarge {
                field: "tracker hash result items",
                len: (result.len() + 1) as u64,
                max: MAX_TRACKER_MATCH_RESULT_ITEMS as u64,
            });
        }
        let info_hash = bounded_tracker_text_column(
            row,
            0,
            "tracker hash info hash",
            MAX_TORRENT_TRACKER_INFO_HASH_BYTES,
        )?;
        bytes = bytes.saturating_add(info_hash.len());
        if bytes > MAX_TRACKER_MATCH_RESULT_BYTES {
            return Err(DbError::ValueTooLarge {
                field: "tracker hash result bytes",
                len: bytes as u64,
                max: MAX_TRACKER_MATCH_RESULT_BYTES as u64,
            });
        }
        result.push(info_hash);
    }
    Ok(result)
}

/// Return torrent hashes whose normalized tracker URL contains `needle`.
/// `instr` is used instead of `LIKE` so tracker text cannot introduce
/// wildcard semantics.  This is the database-side half of TorrentNG automation
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
        "SELECT info_hash,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(length(CAST(info_hash AS BLOB))) OVER (), 0)
                    AS result_bytes
         FROM (
             SELECT DISTINCT info_hash
             FROM torrent_trackers
             WHERE instr(lower(url), lower(?1)) > 0
         )
         ORDER BY info_hash ASC",
    )?;
    let mut rows = stmt.query(params![needle])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(1)?.max(0) as u64;
    let result_bytes = first.get::<_, i64>(2)?.max(0) as u64;
    if result_count > MAX_TRACKER_MATCH_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker match result items",
            len: result_count,
            max: MAX_TRACKER_MATCH_RESULT_ITEMS as u64,
        });
    }
    if result_bytes > MAX_TRACKER_MATCH_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker match result bytes",
            len: result_bytes,
            max: MAX_TRACKER_MATCH_RESULT_BYTES as u64,
        });
    }

    let mut result = Vec::with_capacity(result_count as usize);
    result.push(bounded_tracker_text_column(
        first,
        0,
        "torrent info hash",
        MAX_TORRENT_TRACKER_INFO_HASH_BYTES,
    )?);
    while let Some(row) = rows.next()? {
        result.push(bounded_tracker_text_column(
            row,
            0,
            "torrent info hash",
            MAX_TORRENT_TRACKER_INFO_HASH_BYTES,
        )?);
    }
    Ok(result)
}

/// Return the count and hash-string footprint of a tracker filter result
/// without materializing the matching hashes. Callers use this to admit the
/// transient set they build while intersecting the live registry.
pub fn torrent_hashes_by_tracker_snapshot_size(
    conn: &Connection,
    needle: &str,
) -> Result<(u64, u64), DbError> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok((0, 0));
    }
    let (count, bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(length(CAST(info_hash AS BLOB))), 0)
         FROM (
             SELECT DISTINCT info_hash
             FROM torrent_trackers
             WHERE instr(lower(url), lower(?1)) > 0
         )",
        params![needle],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let count = count.max(0) as u64;
    let bytes = bytes.max(0) as u64;
    if count > MAX_TRACKER_MATCH_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker match result items",
            len: count,
            max: MAX_TRACKER_MATCH_RESULT_ITEMS as u64,
        });
    }
    if bytes > MAX_TRACKER_MATCH_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker match result bytes",
            len: bytes,
            max: MAX_TRACKER_MATCH_RESULT_BYTES as u64,
        });
    }
    Ok((count, bytes))
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
        Ok((
            bounded_tracker_text_column(row, 0, "tracker status", MAX_TORRENT_TRACKER_TEXT_BYTES)?,
            row.get::<_, i64>(1)?,
        ))
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

/// Return tracker status counts for one torrent without materializing any
/// tracker text. This is used by diagnostics, which only need the aggregate
/// counts and must not load optional failure messages or tracker IDs.
pub fn torrent_tracker_status_counts_for_torrent(
    conn: &Connection,
    info_hash: &str,
) -> Result<TorrentTrackerStatusCounts, DbError> {
    let (total, working, warning, error): (i64, i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN status = 'working' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'warning' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0)
         FROM torrent_trackers
         WHERE info_hash = ?1",
        params![info_hash],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    Ok(TorrentTrackerStatusCounts {
        total: total.max(0) as u64,
        working: working.max(0) as u64,
        warning: warning.max(0) as u64,
        error: error.max(0) as u64,
    })
}

/// Aggregate normalized tracker state without materializing every tracker
/// row in the API process. The inner query collapses duplicate tracker URLs
/// within a torrent before the outer query sums peer counts. The windowed
/// result totals are read from the first row before any tracker text is
/// cloned, so a damaged or unexpectedly large database fails closed without
/// building an unbounded Rust result.
pub fn torrent_tracker_health(conn: &Connection) -> Result<Vec<TorrentTrackerHealthRow>, DbError> {
    let mut stmt = conn.prepare(
        "WITH tracker_health AS (
             SELECT tracker,
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
                        -- `completed_at` is historical and can survive a
                        -- recheck that returns a torrent to downloading. Use
                        -- the live amount-left invariant for current progress.
                        CASE WHEN t.state IN ('seeding', 'completed')
                                  OR (t.total_length > 0 AND t.amount_left = 0)
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
         )
         SELECT tracker,
                torrent_count,
                active_count,
                complete_count,
                error_count,
                seed_count,
                peer_count,
                last_updated,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(length(CAST(tracker AS BLOB))) OVER (), 0)
                    AS result_bytes
         FROM tracker_health
         ORDER BY error_count DESC, torrent_count DESC, tracker COLLATE NOCASE",
    )?;
    let mut rows = stmt.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(8)?.max(0) as u64;
    let result_bytes = first.get::<_, i64>(9)?.max(0) as u64;
    if result_count > MAX_TORRENT_TRACKER_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker health result items",
            len: result_count,
            max: MAX_TORRENT_TRACKER_RESULT_ITEMS as u64,
        });
    }
    if result_bytes > MAX_TORRENT_TRACKER_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker health result bytes",
            len: result_bytes,
            max: MAX_TORRENT_TRACKER_RESULT_BYTES as u64,
        });
    }

    let mut result = Vec::with_capacity(result_count as usize);
    result.push(torrent_tracker_health_from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(torrent_tracker_health_from_row(row)?);
    }
    Ok(result)
}

/// Return the count and tracker-URL footprint of the grouped health result
/// without materializing the aggregate rows. This is the admission half of
/// the tracker-health API projection.
pub fn torrent_tracker_health_snapshot_size(conn: &Connection) -> Result<(u64, u64), DbError> {
    let (count, bytes): (i64, i64) = conn.query_row(
        "WITH tracker_urls AS (
             SELECT tt.url AS tracker
             FROM torrent_trackers AS tt
             INNER JOIN torrents AS t ON t.info_hash = tt.info_hash
             WHERE tt.url <> ''
             GROUP BY tt.url, tt.info_hash
         ), grouped_trackers AS (
             SELECT tracker
             FROM tracker_urls
             GROUP BY tracker
         )
         SELECT COUNT(*),
                COALESCE(SUM(length(CAST(tracker AS BLOB))), 0)
         FROM grouped_trackers",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let count = count.max(0) as u64;
    let bytes = bytes.max(0) as u64;
    if count > MAX_TORRENT_TRACKER_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker health result items",
            len: count,
            max: MAX_TORRENT_TRACKER_RESULT_ITEMS as u64,
        });
    }
    if bytes > MAX_TORRENT_TRACKER_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "tracker health result bytes",
            len: bytes,
            max: MAX_TORRENT_TRACKER_RESULT_BYTES as u64,
        });
    }
    Ok((count, bytes))
}

fn torrent_tracker_health_from_row(row: &Row<'_>) -> rusqlite::Result<TorrentTrackerHealthRow> {
    let to_u64 = |value: i64| u64::try_from(value.max(0)).unwrap_or(0);
    Ok(TorrentTrackerHealthRow {
        tracker: bounded_tracker_text_column(row, 0, "tracker URL", MAX_TORRENT_TRACKER_URL_BYTES)?,
        torrent_count: to_u64(row.get(1)?),
        active_count: to_u64(row.get(2)?),
        complete_count: to_u64(row.get(3)?),
        error_count: to_u64(row.get(4)?),
        seed_count: to_u64(row.get(5)?),
        peer_count: to_u64(row.get(6)?),
        last_updated: row.get(7)?,
    })
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
            amount_left: 100,
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
    fn file_writes_reject_oversized_paths_before_replacing_existing_rows() {
        let mut conn = setup();
        let info_hash = "a".repeat(40);
        replace_torrent_files(
            &mut conn,
            &info_hash,
            &[TorrentFileRow {
                info_hash: info_hash.clone(),
                file_index: 0,
                path: "existing.bin".into(),
                length: 100,
                offset: 0,
                priority: 1,
                wanted: true,
                completed_bytes: 50,
            }],
        )
        .unwrap();

        let error = replace_torrent_files(
            &mut conn,
            &info_hash,
            &[TorrentFileRow {
                info_hash: info_hash.clone(),
                file_index: 0,
                path: "x".repeat(MAX_TORRENT_FILE_PATH_BYTES + 1),
                length: 100,
                offset: 0,
                priority: 1,
                wanted: true,
                completed_bytes: 50,
            }],
        )
        .unwrap_err();
        assert!(matches!(error, DbError::ValueTooLarge { .. }));

        let files = list_torrent_files(&conn, &info_hash).unwrap();
        assert_eq!(files[0].path, "existing.bin");
    }

    #[test]
    fn file_reads_reject_oversized_paths_before_materializing_them() {
        let conn = setup();
        let info_hash = "a".repeat(40);
        conn.execute(
            "INSERT INTO torrent_files
                (info_hash, file_index, path, length, offset, priority, wanted, completed_bytes)
             VALUES (?1, 0, ?2, 100, 0, 1, 1, 50)",
            params![&info_hash, "x".repeat(MAX_TORRENT_FILE_PATH_BYTES + 1)],
        )
        .unwrap();

        assert!(list_torrent_files(&conn, &info_hash).is_err());
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
        assert!(torrent_tracker_rows_exist(&conn, &"a".repeat(40)).unwrap());
        let trackers = list_torrent_trackers(&conn, &"a".repeat(40)).unwrap();
        assert_eq!(trackers.len(), 1);
        assert_eq!(
            torrent_tracker_projection(&conn, &"a".repeat(40)).unwrap(),
            Some(("http://tracker/announce".to_owned(), 1))
        );
        assert_eq!(
            list_torrent_tracker_urls(&conn, &"a".repeat(40)).unwrap(),
            vec!["http://tracker/announce"]
        );
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
        assert_eq!(
            torrent_hashes_by_tracker_snapshot_size(&conn, "tracker/announce").unwrap(),
            (1, 40)
        );
        assert_eq!(
            list_torrent_hashes_by_tracker(&conn, "TRACKER/ANNOUNCE").unwrap(),
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
    fn tracker_reads_reject_oversized_columns_before_materializing_them() {
        let conn = setup();
        let info_hash = "a".repeat(40);
        conn.execute(
            "INSERT INTO torrent_trackers
                (info_hash, tracker_index, tier, url, status, failure_reason, warning_message)
             VALUES (?1, 0, 0, ?2, 'working', NULL, NULL)",
            params![&info_hash, "https://tracker.example/announce"],
        )
        .unwrap();

        conn.execute(
            "UPDATE torrent_trackers SET url = ?1 WHERE info_hash = ?2",
            params!["x".repeat(MAX_TORRENT_TRACKER_URL_BYTES + 1), &info_hash],
        )
        .unwrap();
        assert!(list_torrent_trackers(&conn, &info_hash).is_err());
        assert!(list_torrent_tracker_urls(&conn, &info_hash).is_err());
        assert!(torrent_tracker_projection(&conn, &info_hash).is_err());
        assert!(torrent_tracker_health(&conn).is_err());

        conn.execute(
            "UPDATE torrent_trackers SET url = ?1, tracker_id = ?2 WHERE info_hash = ?3",
            params![
                "https://tracker.example/announce",
                vec![0xffu8; MAX_TORRENT_TRACKER_ID_BYTES + 1],
                &info_hash
            ],
        )
        .unwrap();
        assert!(list_torrent_trackers(&conn, &info_hash).is_err());
        assert_eq!(
            list_torrent_tracker_urls(&conn, &info_hash).unwrap(),
            vec!["https://tracker.example/announce"]
        );
        assert_eq!(
            torrent_tracker_projection(&conn, &info_hash).unwrap(),
            Some(("https://tracker.example/announce".to_owned(), 1))
        );

        conn.execute(
            "UPDATE torrent_trackers
             SET tracker_id = NULL, failure_reason = ?1
             WHERE info_hash = ?2",
            params!["x".repeat(MAX_TORRENT_TRACKER_TEXT_BYTES + 1), &info_hash],
        )
        .unwrap();
        assert!(list_torrent_trackers(&conn, &info_hash).is_err());

        conn.execute(
            "UPDATE torrent_trackers
             SET failure_reason = NULL, warning_message = ?1
             WHERE info_hash = ?2",
            params!["x".repeat(MAX_TORRENT_TRACKER_TEXT_BYTES + 1), &info_hash],
        )
        .unwrap();
        assert!(list_torrent_trackers(&conn, &info_hash).is_err());

        conn.execute(
            "UPDATE torrent_trackers
             SET warning_message = NULL, status = ?1
             WHERE info_hash = ?2",
            params!["x".repeat(MAX_TORRENT_TRACKER_TEXT_BYTES + 1), &info_hash],
        )
        .unwrap();
        assert!(torrent_tracker_status_counts(&conn).is_err());
    }

    #[test]
    fn tracker_health_rejects_an_oversized_result_before_materializing_it() {
        let mut conn = setup();
        let info_hash = "a".repeat(40);
        let trackers = (0..=MAX_TORRENT_TRACKER_RESULT_BYTES / MAX_TORRENT_TRACKER_URL_BYTES)
            .map(|tracker_index| TorrentTrackerRow {
                info_hash: info_hash.clone(),
                tracker_index: tracker_index as i64,
                tier: 0,
                url: format!(
                    "{tracker_index:04}{}",
                    "x".repeat(MAX_TORRENT_TRACKER_URL_BYTES - 4)
                ),
                tracker_id: None,
                status: "working".into(),
                last_announce_at: None,
                next_announce_at: None,
                last_success_at: None,
                failure_reason: None,
                warning_message: None,
                seeders: None,
                leechers: None,
                completed: None,
                uploaded: 0,
                downloaded: 0,
                left_bytes: 0,
            })
            .collect::<Vec<_>>();
        replace_torrent_trackers(&mut conn, &info_hash, &trackers).unwrap();

        assert!(matches!(
            torrent_tracker_health(&conn),
            Err(DbError::ValueTooLarge {
                field: "tracker health result bytes",
                ..
            })
        ));
    }

    #[test]
    fn tracker_startup_aggregates_group_deadlines_and_hashes() {
        let mut conn = setup();
        let info_hash = "a".repeat(40);
        let tracker = |tracker_index: i64, next_announce_at: Option<i64>| TorrentTrackerRow {
            info_hash: info_hash.clone(),
            tracker_index,
            tier: 0,
            url: format!("https://tracker-{tracker_index}.example/announce"),
            tracker_id: None,
            status: "working".into(),
            last_announce_at: None,
            next_announce_at,
            last_success_at: None,
            failure_reason: None,
            warning_message: None,
            seeders: None,
            leechers: None,
            completed: None,
            uploaded: 0,
            downloaded: 0,
            left_bytes: 0,
        };
        replace_torrent_trackers(
            &mut conn,
            &info_hash,
            &[
                tracker(0, Some(200)),
                tracker(1, Some(100)),
                tracker(2, None),
            ],
        )
        .unwrap();

        assert_eq!(
            list_torrent_tracker_deadlines(&conn).unwrap(),
            vec![(info_hash.clone(), 100)]
        );
        assert_eq!(list_torrent_tracker_hashes(&conn).unwrap(), vec![info_hash]);
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
        // A recheck can leave this historical timestamp populated while the
        // live payload is incomplete. Tracker aggregates must not count it.
        second.completed_at = Some(30);
        second.amount_left = 50;
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
        assert_eq!(
            torrent_tracker_health_snapshot_size(&conn).unwrap(),
            (1, url.len() as u64)
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
