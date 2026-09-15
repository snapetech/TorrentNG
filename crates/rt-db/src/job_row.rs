use rusqlite::{
    params,
    types::{Type, ValueRef},
    Connection, Row, Transaction,
};
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::{fmt, io};

use crate::error::DbError;

pub const MAX_JOB_AFFECTED_TORRENTS: usize = 256;
pub const MAX_JOB_AFFECTED_TORRENTS_BYTES: usize = 64 * 1024;
pub const MAX_JOB_INVALID_PIECES: usize = 32 * 1024;
pub const MAX_JOB_INVALID_PIECES_BYTES: usize = 1024 * 1024;
pub const MAX_JOB_RESULT_ITEMS: usize = 1_024;
pub const MAX_JOB_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_JOB_ID_BYTES: usize = 256;
const MAX_JOB_KIND_BYTES: usize = 256;
const MAX_JOB_STATE_BYTES: usize = 64;
const MAX_JOB_ERROR_BYTES: usize = 64 * 1024;
const MAX_JOB_AFFECTED_TORRENT_BYTES: usize = 64;

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
        Ok(JobRow {
            job_id: bounded_job_text(row, 0, "job id", MAX_JOB_ID_BYTES)?,
            kind: bounded_job_text(row, 1, "job kind", MAX_JOB_KIND_BYTES)?,
            state: bounded_job_text(row, 2, "job state", MAX_JOB_STATE_BYTES)?,
            dry_run: row.get::<_, i64>(3)? != 0,
            affected_torrents: decode_bounded_string_vec_column(
                row,
                4,
                "job affected torrents",
                MAX_JOB_AFFECTED_TORRENTS_BYTES,
                MAX_JOB_AFFECTED_TORRENTS,
                MAX_JOB_AFFECTED_TORRENT_BYTES,
            )?,
            total: row.get(5)?,
            done: row.get(6)?,
            checkpoint: row.get(7)?,
            file_index: row.get(8)?,
            piece_index: row.get(9)?,
            byte_offset: row.get(10)?,
            verified_bytes: row.get(11)?,
            invalid_pieces: decode_bounded_i64_vec_column(
                row,
                12,
                "job invalid pieces",
                MAX_JOB_INVALID_PIECES_BYTES,
                MAX_JOB_INVALID_PIECES,
            )?,
            error: optional_bounded_job_text(row, 13, "job error", MAX_JOB_ERROR_BYTES)?,
            created_at: row.get(14)?,
            started_at: row.get(15)?,
            updated_at: row.get(16)?,
            finished_at: row.get(17)?,
        })
    }
}

fn job_column_value_error(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn job_text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(job_column_value_error(
            column,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_job_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = job_text_value(row, column, field)? else {
        return Err(job_column_value_error(column, format!("{field} is NULL")));
    };
    if value.len() > maximum {
        return Err(job_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| job_column_value_error(column, format!("{field} is not UTF-8: {error}")))
}

fn optional_bounded_job_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<Option<String>> {
    let Some(value) = job_text_value(row, column, field)? else {
        return Ok(None);
    };
    if value.len() > maximum {
        return Err(job_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(|value| Some(value.to_owned()))
        .map_err(|error| job_column_value_error(column, format!("{field} is not UTF-8: {error}")))
}

struct BoundedStringVecVisitor {
    field: &'static str,
    max_items: usize,
    max_item_bytes: usize,
}

impl<'de> Visitor<'de> for BoundedStringVecVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON array of strings")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values =
            Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(self.max_items));
        while let Some(value) = sequence.next_element::<String>()? {
            if values.len() >= self.max_items {
                return Err(de::Error::custom(format!(
                    "{} contains more than {} items",
                    self.field, self.max_items
                )));
            }
            if value.len() > self.max_item_bytes {
                return Err(de::Error::custom(format!(
                    "{} item contains {} bytes; maximum is {}",
                    self.field,
                    value.len(),
                    self.max_item_bytes
                )));
            }
            values.push(value);
        }
        Ok(values)
    }
}

struct BoundedI64VecVisitor {
    field: &'static str,
    max_items: usize,
}

impl<'de> Visitor<'de> for BoundedI64VecVisitor {
    type Value = Vec<i64>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON array of integers")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values =
            Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(self.max_items));
        while let Some(value) = sequence.next_element::<i64>()? {
            if values.len() >= self.max_items {
                return Err(de::Error::custom(format!(
                    "{} contains more than {} items",
                    self.field, self.max_items
                )));
            }
            values.push(value);
        }
        Ok(values)
    }
}

fn bounded_job_json_text<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<&'a str> {
    let Some(value) = job_text_value(row, column, field)? else {
        return Err(job_column_value_error(column, format!("{field} is NULL")));
    };
    if value.len() > maximum {
        return Err(job_column_value_error(
            column,
            format!(
                "{field} JSON is {} bytes; maximum is {maximum}",
                value.len()
            ),
        ));
    }
    std::str::from_utf8(value).map_err(|error| {
        job_column_value_error(column, format!("{field} JSON is not UTF-8: {error}"))
    })
}

fn decode_bounded_string_vec_column(
    row: &Row<'_>,
    column: usize,
    field: &'static str,
    max_json_bytes: usize,
    max_items: usize,
    max_item_bytes: usize,
) -> rusqlite::Result<Vec<String>> {
    let value = bounded_job_json_text(row, column, field, max_json_bytes)?;
    let mut deserializer = serde_json::Deserializer::from_str(value);
    let decoded = deserializer
        .deserialize_seq(BoundedStringVecVisitor {
            field,
            max_items,
            max_item_bytes,
        })
        .map_err(|error| {
            job_column_value_error(column, format!("{field} JSON is invalid: {error}"))
        })?;
    deserializer.end().map_err(|error| {
        job_column_value_error(column, format!("{field} JSON has trailing data: {error}"))
    })?;
    Ok(decoded)
}

fn decode_bounded_i64_vec_column(
    row: &Row<'_>,
    column: usize,
    field: &'static str,
    max_json_bytes: usize,
    max_items: usize,
) -> rusqlite::Result<Vec<i64>> {
    let value = bounded_job_json_text(row, column, field, max_json_bytes)?;
    let mut deserializer = serde_json::Deserializer::from_str(value);
    let decoded = deserializer
        .deserialize_seq(BoundedI64VecVisitor { field, max_items })
        .map_err(|error| {
            job_column_value_error(column, format!("{field} JSON is invalid: {error}"))
        })?;
    deserializer.end().map_err(|error| {
        job_column_value_error(column, format!("{field} JSON has trailing data: {error}"))
    })?;
    Ok(decoded)
}

fn validate_job_text(value: &str, maximum: usize, field: &'static str) -> Result<(), DbError> {
    if value.len() > maximum {
        return Err(DbError::ValueTooLarge {
            field,
            len: value.len() as u64,
            max: maximum as u64,
        });
    }
    Ok(())
}

fn validate_job(job: &JobRow) -> Result<(), DbError> {
    validate_job_text(&job.job_id, MAX_JOB_ID_BYTES, "job id")?;
    validate_job_text(&job.kind, MAX_JOB_KIND_BYTES, "job kind")?;
    validate_job_text(&job.state, MAX_JOB_STATE_BYTES, "job state")?;
    if job.affected_torrents.len() > MAX_JOB_AFFECTED_TORRENTS {
        return Err(DbError::ValueTooLarge {
            field: "job affected torrents",
            len: job.affected_torrents.len() as u64,
            max: MAX_JOB_AFFECTED_TORRENTS as u64,
        });
    }
    let affected_bytes = job
        .affected_torrents
        .iter()
        .map(String::len)
        .fold(0usize, usize::saturating_add);
    if affected_bytes > MAX_JOB_AFFECTED_TORRENTS_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "job affected torrent bytes",
            len: affected_bytes as u64,
            max: MAX_JOB_AFFECTED_TORRENTS_BYTES as u64,
        });
    }
    for torrent in &job.affected_torrents {
        validate_job_text(
            torrent,
            MAX_JOB_AFFECTED_TORRENT_BYTES,
            "job affected torrent",
        )?;
    }
    if job.invalid_pieces.len() > MAX_JOB_INVALID_PIECES {
        return Err(DbError::ValueTooLarge {
            field: "job invalid pieces",
            len: job.invalid_pieces.len() as u64,
            max: MAX_JOB_INVALID_PIECES as u64,
        });
    }
    if let Some(error) = &job.error {
        validate_job_text(error, MAX_JOB_ERROR_BYTES, "job error")?;
    }
    Ok(())
}

pub fn upsert_job(conn: &Connection, job: &JobRow) -> Result<(), DbError> {
    validate_job(job)?;
    let affected_json = serde_json::to_string(&job.affected_torrents)?;
    let invalid_json = serde_json::to_string(&job.invalid_pieces)?;
    validate_job_text(
        &affected_json,
        MAX_JOB_AFFECTED_TORRENTS_BYTES,
        "job affected torrents JSON",
    )?;
    validate_job_text(
        &invalid_json,
        MAX_JOB_INVALID_PIECES_BYTES,
        "job invalid pieces JSON",
    )?;
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

/// Upsert a job inside a caller-owned transaction. Job state and its event
/// must commit together; otherwise a crash can expose a new state without the
/// event that explains how the state was reached.
pub fn upsert_job_in_tx(tx: &Transaction<'_>, job: &JobRow) -> Result<(), DbError> {
    validate_job(job)?;
    let affected_json = serde_json::to_string(&job.affected_torrents)?;
    let invalid_json = serde_json::to_string(&job.invalid_pieces)?;
    validate_job_text(
        &affected_json,
        MAX_JOB_AFFECTED_TORRENTS_BYTES,
        "job affected torrents JSON",
    )?;
    validate_job_text(
        &invalid_json,
        MAX_JOB_INVALID_PIECES_BYTES,
        "job invalid pieces JSON",
    )?;
    tx.execute(
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
                invalid_pieces, error, created_at, started_at, updated_at, finished_at,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(job_id AS BLOB)), 0) +
                    COALESCE(length(CAST(kind AS BLOB)), 0) +
                    COALESCE(length(CAST(state AS BLOB)), 0) +
                    COALESCE(length(CAST(affected_torrents AS BLOB)), 0) +
                    COALESCE(length(CAST(invalid_pieces AS BLOB)), 0) +
                    COALESCE(length(CAST(error AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM jobs
         WHERE state IN ('queued', 'running', 'paused', 'cancelling', 'commit_pending')
         ORDER BY updated_at DESC
         LIMIT ?1",
    )?;
    let mut rows = stmt.query(params![MAX_JOB_RESULT_ITEMS as i64])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = validate_job_result_window(first)?;
    let mut result = Vec::with_capacity(result_count);
    result.push(JobRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(JobRow::from_row(row)?);
    }
    Ok(result)
}

/// Return failed jobs whose error begins with a durable, caller-defined
/// marker. Prefix matching avoids treating arbitrary failed jobs as recovery
/// blockers while keeping the job state itself terminal.
pub fn list_failed_jobs_with_error_prefix(
    conn: &Connection,
    kind: &str,
    prefix: &str,
) -> Result<Vec<JobRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT job_id, kind, state, dry_run, affected_torrents, total, done,
                checkpoint, file_index, piece_index, byte_offset, verified_bytes,
                invalid_pieces, error, created_at, started_at, updated_at, finished_at,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(job_id AS BLOB)), 0) +
                    COALESCE(length(CAST(kind AS BLOB)), 0) +
                    COALESCE(length(CAST(state AS BLOB)), 0) +
                    COALESCE(length(CAST(affected_torrents AS BLOB)), 0) +
                    COALESCE(length(CAST(invalid_pieces AS BLOB)), 0) +
                    COALESCE(length(CAST(error AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM jobs
         WHERE kind = ?1
           AND state = 'failed'
           AND error IS NOT NULL
           AND substr(error, 1, length(?2)) = ?2
         ORDER BY updated_at DESC
         LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![kind, prefix, MAX_JOB_RESULT_ITEMS as i64])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = validate_job_result_window(first)?;
    let mut result = Vec::with_capacity(result_count);
    result.push(JobRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(JobRow::from_row(row)?);
    }
    Ok(result)
}

fn validate_job_result_window(row: &Row<'_>) -> Result<usize, DbError> {
    let result_count = row.get::<_, i64>(18)?;
    if result_count < 0 || result_count as u64 > MAX_JOB_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "active job result",
            len: result_count.max(0) as u64,
            max: MAX_JOB_RESULT_ITEMS as u64,
        });
    }
    let result_bytes = row.get::<_, i64>(19)?;
    if result_bytes < 0 || result_bytes as u64 > MAX_JOB_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "job result bytes",
            len: result_bytes.max(0) as u64,
            max: MAX_JOB_RESULT_BYTES as u64,
        });
    }
    Ok(result_count as usize)
}

/// Count non-terminal jobs without materializing their payloads. This is the
/// hot-path counterpart to `list_active_jobs`, used by engine statistics.
pub fn count_active_jobs(conn: &Connection) -> Result<u64, DbError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM jobs
         WHERE state IN ('queued', 'running', 'paused', 'cancelling', 'commit_pending')",
        [],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
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
        let mut commit_pending = sample();
        commit_pending.job_id = "job-3".into();
        commit_pending.state = "commit_pending".into();
        commit_pending.finished_at = None;
        upsert_job(&conn, &commit_pending).unwrap();
        let active = list_active_jobs(&conn).unwrap();
        assert_eq!(active.len(), 2);
        assert!(active.iter().any(|job| job.job_id == "job-1"));
        assert!(active.iter().any(|job| job.job_id == "job-3"));
        assert_eq!(count_active_jobs(&conn).unwrap(), 2);
    }

    #[test]
    fn list_failed_jobs_with_error_prefix_is_narrow() {
        let conn = setup();
        let mut manual = sample();
        manual.job_id = "manual".into();
        manual.kind = "storage_plan".into();
        manual.state = "failed".into();
        manual.error = Some("manual recovery required: ambiguous filesystem".into());
        manual.finished_at = Some(30);
        upsert_job(&conn, &manual).unwrap();

        let mut ordinary = manual.clone();
        ordinary.job_id = "ordinary".into();
        ordinary.error = Some("storage plan failed".into());
        upsert_job(&conn, &ordinary).unwrap();

        let jobs =
            list_failed_jobs_with_error_prefix(&conn, "storage_plan", "manual recovery required: ")
                .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_id, "manual");
    }

    #[test]
    fn active_job_result_rejects_oversized_aggregate_before_materializing_it() {
        let conn = setup();
        let payload = "0".repeat(MAX_JOB_RESULT_BYTES / 2 + 1);
        for job_id in ["large-1", "large-2"] {
            conn.execute(
                "INSERT INTO jobs (job_id, kind, state, invalid_pieces, created_at, updated_at)
                 VALUES (?1, 'recheck_torrent', 'running', ?2, 1, 1)",
                params![job_id, payload],
            )
            .unwrap();
        }

        assert!(matches!(
            list_active_jobs(&conn),
            Err(DbError::ValueTooLarge {
                field: "job result bytes",
                ..
            })
        ));
    }

    #[test]
    fn corrupt_job_checkpoint_json_fails_closed() {
        let conn = setup();
        let row = sample();
        upsert_job(&conn, &row).unwrap();
        conn.execute("UPDATE jobs SET invalid_pieces='not-json'", [])
            .unwrap();

        assert!(get_job(&conn, &row.job_id).is_err());
        assert!(list_active_jobs(&conn).is_err());
    }

    #[test]
    fn job_vector_limits_reject_oversized_writes() {
        let conn = setup();
        let mut row = sample();
        row.affected_torrents = vec!["a".repeat(40); MAX_JOB_AFFECTED_TORRENTS + 1];
        let error = upsert_job(&conn, &row).unwrap_err();
        assert!(matches!(
            error,
            DbError::ValueTooLarge {
                field: "job affected torrents",
                ..
            }
        ));

        row = sample();
        row.invalid_pieces = vec![1; MAX_JOB_INVALID_PIECES + 1];
        let error = upsert_job(&conn, &row).unwrap_err();
        assert!(matches!(
            error,
            DbError::ValueTooLarge {
                field: "job invalid pieces",
                ..
            }
        ));
    }

    #[test]
    fn job_vector_reads_reject_oversized_json_before_deserializing_it() {
        let conn = setup();
        let row = sample();
        upsert_job(&conn, &row).unwrap();
        let too_many_torrents =
            serde_json::to_string(&vec!["a"; MAX_JOB_AFFECTED_TORRENTS + 1]).unwrap();
        conn.execute(
            "UPDATE jobs SET affected_torrents = ?1 WHERE job_id = 'job-1'",
            params![too_many_torrents],
        )
        .unwrap();
        assert!(get_job(&conn, &row.job_id).is_err());
        assert!(list_active_jobs(&conn).is_err());

        let too_large_invalid = "x".repeat(MAX_JOB_INVALID_PIECES_BYTES + 1);
        conn.execute(
            "UPDATE jobs SET invalid_pieces = ?1 WHERE job_id = 'job-1'",
            params![too_large_invalid],
        )
        .unwrap();
        assert!(get_job(&conn, &row.job_id).is_err());
    }
}
