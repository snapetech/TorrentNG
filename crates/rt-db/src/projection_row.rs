//! Durable records for startup projection-reconciliation findings.

use rusqlite::{
    params,
    types::{Type, ValueRef},
    Connection, Row, Transaction,
};
use serde::{Deserialize, Serialize};
use std::io;

use crate::error::DbError;

pub const MAX_PROJECTION_ISSUE_RESULT_ITEMS: usize = 4_096;
pub const MAX_PROJECTION_ISSUE_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROJECTION_INFO_HASH_BYTES: usize = 64;
const MAX_PROJECTION_ARTIFACT_BYTES: usize = 256;
const MAX_PROJECTION_PATH_BYTES: usize = 16 * 1024;
const MAX_PROJECTION_REASON_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectionIssueRow {
    pub issue_id: Option<i64>,
    pub info_hash: Option<String>,
    pub artifact: String,
    pub path: Option<String>,
    pub reason: String,
    pub detected_at: i64,
    pub resolved_at: Option<i64>,
}

impl ProjectionIssueRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let info_hash = bounded_projection_text(
            row,
            1,
            "projection issue info hash",
            MAX_PROJECTION_INFO_HASH_BYTES,
        )?;
        let path =
            bounded_projection_text(row, 3, "projection issue path", MAX_PROJECTION_PATH_BYTES)?;
        Ok(Self {
            issue_id: Some(row.get(0)?),
            info_hash: (!info_hash.is_empty()).then_some(info_hash),
            artifact: bounded_projection_text(
                row,
                2,
                "projection issue artifact",
                MAX_PROJECTION_ARTIFACT_BYTES,
            )?,
            path: (!path.is_empty()).then_some(path),
            reason: bounded_projection_text(
                row,
                4,
                "projection issue reason",
                MAX_PROJECTION_REASON_BYTES,
            )?,
            detected_at: row.get(5)?,
            resolved_at: row.get(6)?,
        })
    }
}

fn projection_column_value_error(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn bounded_projection_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let value = match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => value,
        ValueRef::Null => {
            return Err(projection_column_value_error(
                column,
                format!("{field} is NULL"),
            ))
        }
        other => {
            return Err(projection_column_value_error(
                column,
                format!("{field} has unexpected SQLite type {other:?}"),
            ))
        }
    };
    if value.len() > maximum {
        return Err(projection_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| {
            projection_column_value_error(column, format!("{field} is not UTF-8: {error}"))
        })
}

fn validate_projection_text(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<(), DbError> {
    if value.len() > maximum {
        return Err(DbError::ValueTooLarge {
            field,
            len: value.len() as u64,
            max: maximum as u64,
        });
    }
    Ok(())
}

fn validate_projection_issue(issue: &ProjectionIssueRow) -> Result<(), DbError> {
    if let Some(info_hash) = issue.info_hash.as_deref() {
        validate_projection_text(
            info_hash,
            "projection issue info hash",
            MAX_PROJECTION_INFO_HASH_BYTES,
        )?;
    }
    validate_projection_text(
        &issue.artifact,
        "projection issue artifact",
        MAX_PROJECTION_ARTIFACT_BYTES,
    )?;
    if let Some(path) = issue.path.as_deref() {
        validate_projection_text(path, "projection issue path", MAX_PROJECTION_PATH_BYTES)?;
    }
    validate_projection_text(
        &issue.reason,
        "projection issue reason",
        MAX_PROJECTION_REASON_BYTES,
    )?;
    Ok(())
}

/// Record an active issue once. Repeated daemon restarts update the reason
/// and detection time without generating an unbounded duplicate trail.
pub fn record_active_issue(conn: &Connection, issue: &ProjectionIssueRow) -> Result<i64, DbError> {
    validate_projection_issue(issue)?;
    conn.execute(
        "INSERT INTO projection_issues
            (info_hash, artifact, path, reason, detected_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)
         ON CONFLICT(info_hash, artifact, path) WHERE resolved_at IS NULL DO UPDATE SET
            reason = excluded.reason,
            detected_at = excluded.detected_at",
        params![
            issue.info_hash.as_deref().unwrap_or_default(),
            issue.artifact,
            issue.path.as_deref().unwrap_or_default(),
            issue.reason,
            issue.detected_at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record an active issue inside the caller's transaction.  Projection
/// reconciliation often changes the durable torrent row at the same time as
/// it records the finding; keeping both operations in one transaction avoids
/// a restart window where the row says `error` but the diagnostic issue was
/// never committed (or vice versa).
pub fn record_active_issue_in_tx(
    tx: &Transaction<'_>,
    issue: &ProjectionIssueRow,
) -> Result<i64, DbError> {
    validate_projection_issue(issue)?;
    tx.execute(
        "INSERT INTO projection_issues
            (info_hash, artifact, path, reason, detected_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)
         ON CONFLICT(info_hash, artifact, path) WHERE resolved_at IS NULL DO UPDATE SET
            reason = excluded.reason,
            detected_at = excluded.detected_at",
        params![
            issue.info_hash.as_deref().unwrap_or_default(),
            issue.artifact,
            issue.path.as_deref().unwrap_or_default(),
            issue.reason,
            issue.detected_at,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn resolve_active_issue(
    conn: &Connection,
    info_hash: Option<&str>,
    artifact: &str,
    path: Option<&str>,
    resolved_at: i64,
) -> Result<usize, DbError> {
    Ok(conn.execute(
        "UPDATE projection_issues
         SET resolved_at = ?1
         WHERE resolved_at IS NULL
           AND info_hash = ?2
           AND artifact = ?3
           AND path = ?4",
        params![
            resolved_at,
            info_hash.unwrap_or_default(),
            artifact,
            path.unwrap_or_default(),
        ],
    )?)
}

/// Resolve an active issue inside the caller's transaction.
pub fn resolve_active_issue_in_tx(
    tx: &Transaction<'_>,
    info_hash: Option<&str>,
    artifact: &str,
    path: Option<&str>,
    resolved_at: i64,
) -> Result<usize, DbError> {
    Ok(tx.execute(
        "UPDATE projection_issues
         SET resolved_at = ?1
         WHERE resolved_at IS NULL
           AND info_hash = ?2
           AND artifact = ?3
           AND path = ?4",
        params![
            resolved_at,
            info_hash.unwrap_or_default(),
            artifact,
            path.unwrap_or_default(),
        ],
    )?)
}

pub fn list_active_issues(conn: &Connection) -> Result<Vec<ProjectionIssueRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT issue_id, info_hash, artifact, path, reason, detected_at, resolved_at,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(info_hash AS BLOB)), 0) +
                    COALESCE(length(CAST(artifact AS BLOB)), 0) +
                    COALESCE(length(CAST(path AS BLOB)), 0) +
                    COALESCE(length(CAST(reason AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM projection_issues
         WHERE resolved_at IS NULL
         ORDER BY issue_id ASC",
    )?;
    let mut rows = stmt.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(7)?;
    if result_count < 0 || result_count as u64 > MAX_PROJECTION_ISSUE_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "projection issue result items",
            len: result_count.max(0) as u64,
            max: MAX_PROJECTION_ISSUE_RESULT_ITEMS as u64,
        });
    }
    let result_bytes = first.get::<_, i64>(8)?;
    if result_bytes < 0 || result_bytes as u64 > MAX_PROJECTION_ISSUE_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "projection issue result bytes",
            len: result_bytes.max(0) as u64,
            max: MAX_PROJECTION_ISSUE_RESULT_BYTES as u64,
        });
    }
    let mut result = Vec::with_capacity(result_count as usize);
    result.push(ProjectionIssueRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(ProjectionIssueRow::from_row(row)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;

    #[test]
    fn active_issue_is_idempotent_and_resolvable() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let issue = ProjectionIssueRow {
            issue_id: None,
            info_hash: Some("a".repeat(40)),
            artifact: "torrent_blob".to_owned(),
            path: Some("a.torrent".to_owned()),
            reason: "missing".to_owned(),
            detected_at: 10,
            resolved_at: None,
        };
        record_active_issue(&conn, &issue).unwrap();
        let mut updated = issue.clone();
        updated.reason = "still missing".to_owned();
        updated.detected_at = 11;
        record_active_issue(&conn, &updated).unwrap();
        let rows = list_active_issues(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "still missing");
        assert_eq!(
            resolve_active_issue(
                &conn,
                Some(&"a".repeat(40)),
                "torrent_blob",
                Some("a.torrent"),
                12
            )
            .unwrap(),
            1
        );
        assert!(list_active_issues(&conn).unwrap().is_empty());
    }

    #[test]
    fn active_issue_writes_reject_oversized_text() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let issue = ProjectionIssueRow {
            issue_id: None,
            info_hash: Some("a".repeat(MAX_PROJECTION_INFO_HASH_BYTES + 1)),
            artifact: "artifact".to_owned(),
            path: None,
            reason: "reason".to_owned(),
            detected_at: 10,
            resolved_at: None,
        };

        assert!(matches!(
            record_active_issue(&conn, &issue),
            Err(DbError::ValueTooLarge { .. })
        ));
    }

    #[test]
    fn active_issue_reads_reject_oversized_legacy_text_before_string_conversion() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO projection_issues
                (info_hash, artifact, path, reason, detected_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                "a".repeat(MAX_PROJECTION_INFO_HASH_BYTES + 1),
                "artifact",
                "",
                "reason",
                10_i64
            ],
        )
        .unwrap();

        assert!(list_active_issues(&conn).is_err());
    }
}
