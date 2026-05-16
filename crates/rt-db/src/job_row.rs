use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobRow {
    pub job_id: String,
    pub kind: String,
    pub state: String,
    pub dry_run: bool,
    pub affected_torrents: Vec<String>,
    pub total: i64,
    pub done: i64,
    pub checkpoint: i64,
    pub file_index: Option<i64>,
    pub piece_index: Option<i64>,
    pub byte_offset: Option<i64>,
    pub verified_bytes: i64,
    pub invalid_pieces: Vec<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

impl JobRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let affected_json: String = row.get(4)?;
        let invalid_json: String = row.get(12)?;
        Ok(JobRow {
            job_id: row.get(0)?,
            kind: row.get(1)?,
            state: row.get(2)?,
            dry_run: row.get::<_, i64>(3)? != 0,
            affected_torrents: serde_json::from_str(&affected_json).unwrap_or_default(),
            total: row.get(5)?,
            done: row.get(6)?,
            checkpoint: row.get(7)?,
            file_index: row.get(8)?,
            piece_index: row.get(9)?,
            byte_offset: row.get(10)?,
            verified_bytes: row.get(11)?,
            invalid_pieces: serde_json::from_str(&invalid_json).unwrap_or_default(),
            error: row.get(13)?,
            created_at: row.get(14)?,
            started_at: row.get(15)?,
            updated_at: row.get(16)?,
            finished_at: row.get(17)?,
        })
    }
}

pub fn upsert_job(conn: &Connection, job: &JobRow) -> Result<(), DbError> {
    let affected_json = serde_json::to_string(&job.affected_torrents)?;
    let invalid_json = serde_json::to_string(&job.invalid_pieces)?;
    conn.execute(
        "INSERT INTO jobs (
            job_id, kind, state, dry_run, affected_torrents, total, done,
            checkpoint, file_index, piece_index, byte_offset, verified_bytes,
            invalid_pieces, error, created_at, started_at, updated_at, finished_at
         )
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
         ON CONFLICT(job_id) DO UPDATE SET
            kind=excluded.kind,
            state=excluded.state,
            dry_run=excluded.dry_run,
            affected_torrents=excluded.affected_torrents,
            total=excluded.total,
            done=excluded.done,
            checkpoint=excluded.checkpoint,
            file_index=excluded.file_index,
            piece_index=excluded.piece_index,
            byte_offset=excluded.byte_offset,
            verified_bytes=excluded.verified_bytes,
            invalid_pieces=excluded.invalid_pieces,
            error=excluded.error,
            started_at=excluded.started_at,
            updated_at=excluded.updated_at,
            finished_at=excluded.finished_at",
        params![
            job.job_id,
            job.kind,
            job.state,
            job.dry_run as i64,
            affected_json,
            job.total,
            job.done,
            job.checkpoint,
            job.file_index,
            job.piece_index,
            job.byte_offset,
            job.verified_bytes,
            invalid_json,
            job.error,
            job.created_at,
            job.started_at,
            job.updated_at,
            job.finished_at,
        ],
    )?;
    Ok(())
}

pub fn get_job(conn: &Connection, job_id: &str) -> Result<JobRow, DbError> {
    conn.query_row(
        "SELECT job_id, kind, state, dry_run, affected_torrents, total, done,
                checkpoint, file_index, piece_index, byte_offset, verified_bytes,
                invalid_pieces, error, created_at, started_at, updated_at, finished_at
         FROM jobs WHERE job_id = ?1",
        params![job_id],
        JobRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(job_id.to_owned()),
        other => DbError::Sqlite(other),
    })
}

pub fn list_active_jobs(conn: &Connection) -> Result<Vec<JobRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT job_id, kind, state, dry_run, affected_torrents, total, done,
                checkpoint, file_index, piece_index, byte_offset, verified_bytes,
                invalid_pieces, error, created_at, started_at, updated_at, finished_at
         FROM jobs
         WHERE state IN ('queued', 'running', 'paused', 'cancelling')
         ORDER BY updated_at DESC",
    )?;
    let rows = stmt
        .query_map([], JobRow::from_row)?
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

    fn sample() -> JobRow {
        JobRow {
            job_id: "job-1".into(),
            kind: "recheck_torrent".into(),
            state: "queued".into(),
            dry_run: false,
            affected_torrents: vec!["a".repeat(40)],
            total: 100,
            done: 0,
            checkpoint: 0,
            file_index: Some(0),
            piece_index: Some(0),
            byte_offset: Some(0),
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: 10,
            started_at: None,
            updated_at: 10,
            finished_at: None,
        }
    }

    #[test]
    fn upsert_and_get_job() {
        let conn = setup();
        let row = sample();
        upsert_job(&conn, &row).unwrap();
        let fetched = get_job(&conn, "job-1").unwrap();
        assert_eq!(fetched.affected_torrents, row.affected_torrents);
        assert_eq!(fetched.piece_index, Some(0));
    }

    #[test]
    fn upsert_updates_checkpoint() {
        let conn = setup();
        let mut row = sample();
        upsert_job(&conn, &row).unwrap();
        row.state = "running".into();
        row.done = 50;
        row.checkpoint = 50;
        row.piece_index = Some(50);
        row.verified_bytes = 4096;
        row.updated_at = 20;
        upsert_job(&conn, &row).unwrap();
        let fetched = get_job(&conn, "job-1").unwrap();
        assert_eq!(fetched.state, "running");
        assert_eq!(fetched.checkpoint, 50);
        assert_eq!(fetched.verified_bytes, 4096);
    }

    #[test]
    fn list_active_jobs_excludes_terminal() {
        let conn = setup();
        let active = sample();
        upsert_job(&conn, &active).unwrap();
        let mut terminal = sample();
        terminal.job_id = "job-2".into();
        terminal.state = "completed".into();
        terminal.finished_at = Some(30);
        upsert_job(&conn, &terminal).unwrap();
        let active = list_active_jobs(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].job_id, "job-1");
    }
}
