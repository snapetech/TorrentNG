use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEventRow {
    pub event_id: Option<i64>,
    pub occurred_at: i64,
    pub info_hash: Option<String>,
    pub kind: String,
    pub message: Option<String>,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobEventRow {
    pub event_id: Option<i64>,
    pub job_id: String,
    pub occurred_at: i64,
    pub kind: String,
    pub message: Option<String>,
    pub payload: String,
}

impl SessionEventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(SessionEventRow {
            event_id: Some(row.get(0)?),
            occurred_at: row.get(1)?,
            info_hash: row.get(2)?,
            kind: row.get(3)?,
            message: row.get(4)?,
            payload: row.get(5)?,
        })
    }
}

impl JobEventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(JobEventRow {
            event_id: Some(row.get(0)?),
            job_id: row.get(1)?,
            occurred_at: row.get(2)?,
            kind: row.get(3)?,
            message: row.get(4)?,
            payload: row.get(5)?,
        })
    }
}

pub fn append_session_event(conn: &Connection, event: &SessionEventRow) -> Result<i64, DbError> {
    conn.execute(
        "INSERT INTO session_events (occurred_at, info_hash, kind, message, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.occurred_at,
            event.info_hash,
            event.kind,
            event.message,
            event.payload,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_session_events(
    conn: &Connection,
    info_hash: Option<&str>,
    limit: usize,
) -> Result<Vec<SessionEventRow>, DbError> {
    let limit = limit.max(1) as i64;
    if let Some(info_hash) = info_hash {
        let mut stmt = conn.prepare(
            "SELECT event_id, occurred_at, info_hash, kind, message, payload
             FROM session_events
             WHERE info_hash = ?1
             ORDER BY event_id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![info_hash, limit], SessionEventRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(rows);
    }

    let mut stmt = conn.prepare(
        "SELECT event_id, occurred_at, info_hash, kind, message, payload
         FROM session_events
         ORDER BY event_id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], SessionEventRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn append_job_event(conn: &Connection, event: &JobEventRow) -> Result<i64, DbError> {
    conn.execute(
        "INSERT INTO job_events (job_id, occurred_at, kind, message, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.job_id,
            event.occurred_at,
            event.kind,
            event.message,
            event.payload,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_job_events(
    conn: &Connection,
    job_id: &str,
    limit: usize,
) -> Result<Vec<JobEventRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT event_id, job_id, occurred_at, kind, message, payload
         FROM job_events
         WHERE job_id = ?1
         ORDER BY event_id DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![job_id, limit.max(1) as i64], JobEventRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{job_row, schema::migrate};
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn append_and_list_session_events() {
        let conn = setup();
        let event = SessionEventRow {
            event_id: None,
            occurred_at: 10,
            info_hash: Some("a".repeat(40)),
            kind: "torrent_added".into(),
            message: Some("added".into()),
            payload: "{\"source\":\"test\"}".into(),
        };
        let id = append_session_event(&conn, &event).unwrap();
        assert_eq!(id, 1);
        let events = list_session_events(&conn, Some(&"a".repeat(40)), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "torrent_added");
    }

    #[test]
    fn append_and_list_job_events() {
        let conn = setup();
        let job = job_row::JobRow {
            job_id: "job-1".into(),
            kind: "recheck_torrent".into(),
            state: "queued".into(),
            dry_run: false,
            affected_torrents: vec!["a".repeat(40)],
            total: 100,
            done: 0,
            checkpoint: 0,
            file_index: None,
            piece_index: None,
            byte_offset: None,
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: 10,
            started_at: None,
            updated_at: 10,
            finished_at: None,
        };
        job_row::upsert_job(&conn, &job).unwrap();
        append_job_event(
            &conn,
            &JobEventRow {
                event_id: None,
                job_id: "job-1".into(),
                occurred_at: 11,
                kind: "job_queued".into(),
                message: None,
                payload: "{}".into(),
            },
        )
        .unwrap();
        let events = list_job_events(&conn, "job-1", 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "job_queued");
    }
}
