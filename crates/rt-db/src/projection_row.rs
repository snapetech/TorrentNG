//! Durable records for startup projection-reconciliation findings.

use rusqlite::{params, Connection, Row, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

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
        let info_hash: String = row.get(1)?;
        let path: String = row.get(3)?;
        Ok(Self {
            issue_id: Some(row.get(0)?),
            info_hash: (!info_hash.is_empty()).then_some(info_hash),
            artifact: row.get(2)?,
            path: (!path.is_empty()).then_some(path),
            reason: row.get(4)?,
            detected_at: row.get(5)?,
            resolved_at: row.get(6)?,
        })
    }
}

/// Record an active issue once. Repeated daemon restarts update the reason
/// and detection time without generating an unbounded duplicate trail.
pub fn record_active_issue(conn: &Connection, issue: &ProjectionIssueRow) -> Result<i64, DbError> {
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
        "SELECT issue_id, info_hash, artifact, path, reason, detected_at, resolved_at
         FROM projection_issues
         WHERE resolved_at IS NULL
         ORDER BY issue_id ASC",
    )?;
    let rows = stmt
        .query_map([], ProjectionIssueRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
}
