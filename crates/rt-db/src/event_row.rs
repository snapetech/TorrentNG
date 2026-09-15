use rusqlite::{
    params, params_from_iter,
    types::{Type, Value, ValueRef},
    Connection, Row, Transaction,
};
use serde::{Deserialize, Serialize};
use std::io;

use crate::error::DbError;

pub const MAX_SESSION_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;
pub const MAX_JOB_EVENT_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_SESSION_EVENT_RESULT_ITEMS: usize = 1_000;
pub const MAX_JOB_EVENT_RESULT_ITEMS: usize = 1_024;
const MAX_EVENT_INFO_HASH_BYTES: usize = 64;
const MAX_EVENT_JOB_ID_BYTES: usize = 256;
const MAX_EVENT_KIND_BYTES: usize = 256;
const MAX_EVENT_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_EVENT_LEVEL_FILTER_ITEMS: usize = 64;

fn sqlite_limit(value: usize) -> i64 {
    i64::try_from(value.max(1)).unwrap_or(i64::MAX)
}

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
            info_hash: optional_bounded_event_text(
                row,
                2,
                "session event info hash",
                MAX_EVENT_INFO_HASH_BYTES,
            )?,
            kind: bounded_event_text(row, 3, "session event kind", MAX_EVENT_KIND_BYTES)?,
            message: optional_bounded_event_text(
                row,
                4,
                "session event message",
                MAX_EVENT_MESSAGE_BYTES,
            )?,
            payload: bounded_event_text(
                row,
                5,
                "session event payload",
                MAX_SESSION_EVENT_PAYLOAD_BYTES,
            )?,
        })
    }
}

impl JobEventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(JobEventRow {
            event_id: Some(row.get(0)?),
            job_id: bounded_event_text(row, 1, "job event job id", MAX_EVENT_JOB_ID_BYTES)?,
            occurred_at: row.get(2)?,
            kind: bounded_event_text(row, 3, "job event kind", MAX_EVENT_KIND_BYTES)?,
            message: optional_bounded_event_text(
                row,
                4,
                "job event message",
                MAX_EVENT_MESSAGE_BYTES,
            )?,
            payload: bounded_event_text(row, 5, "job event payload", MAX_JOB_EVENT_PAYLOAD_BYTES)?,
        })
    }
}

fn event_column_value_error(
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

fn event_text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(event_column_value_error(
            column,
            Type::Text,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_event_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = event_text_value(row, column, field)? else {
        return Err(event_column_value_error(
            column,
            Type::Text,
            format!("{field} is NULL"),
        ));
    };
    if value.len() > maximum {
        return Err(event_column_value_error(
            column,
            Type::Text,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| {
            event_column_value_error(column, Type::Text, format!("{field} is not UTF-8: {error}"))
        })
}

fn optional_bounded_event_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<Option<String>> {
    let Some(value) = event_text_value(row, column, field)? else {
        return Ok(None);
    };
    if value.len() > maximum {
        return Err(event_column_value_error(
            column,
            Type::Text,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(|value| Some(value.to_owned()))
        .map_err(|error| {
            event_column_value_error(column, Type::Text, format!("{field} is not UTF-8: {error}"))
        })
}

fn validate_event_text(value: &str, maximum: usize, field: &'static str) -> Result<(), DbError> {
    if value.len() > maximum {
        return Err(DbError::ValueTooLarge {
            field,
            len: value.len() as u64,
            max: maximum as u64,
        });
    }
    Ok(())
}

fn validate_optional_event_text(
    value: Option<&str>,
    maximum: usize,
    field: &'static str,
) -> Result<(), DbError> {
    if let Some(value) = value {
        validate_event_text(value, maximum, field)?;
    }
    Ok(())
}

fn validate_session_event(event: &SessionEventRow) -> Result<(), DbError> {
    validate_optional_event_text(
        event.info_hash.as_deref(),
        MAX_EVENT_INFO_HASH_BYTES,
        "session event info hash",
    )?;
    validate_event_text(&event.kind, MAX_EVENT_KIND_BYTES, "session event kind")?;
    validate_optional_event_text(
        event.message.as_deref(),
        MAX_EVENT_MESSAGE_BYTES,
        "session event message",
    )?;
    validate_event_text(
        &event.payload,
        MAX_SESSION_EVENT_PAYLOAD_BYTES,
        "session event payload",
    )?;
    serde_json::from_str::<serde_json::Value>(&event.payload)?;
    Ok(())
}

fn validate_job_event(event: &JobEventRow) -> Result<(), DbError> {
    validate_event_text(&event.job_id, MAX_EVENT_JOB_ID_BYTES, "job event job id")?;
    validate_event_text(&event.kind, MAX_EVENT_KIND_BYTES, "job event kind")?;
    validate_optional_event_text(
        event.message.as_deref(),
        MAX_EVENT_MESSAGE_BYTES,
        "job event message",
    )?;
    validate_event_text(
        &event.payload,
        MAX_JOB_EVENT_PAYLOAD_BYTES,
        "job event payload",
    )?;
    serde_json::from_str::<serde_json::Value>(&event.payload)?;
    Ok(())
}

pub fn append_session_event(conn: &Connection, event: &SessionEventRow) -> Result<i64, DbError> {
    // Session-event payloads are projected by every API surface. Reject bad
    // JSON at the write boundary so a later read cannot turn durable
    // corruption into a dropped or empty-looking event.
    validate_session_event(event)?;
    let level = session_event_level(event);
    conn.execute(
        "INSERT INTO session_events (occurred_at, info_hash, kind, message, payload, level)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.occurred_at,
            event.info_hash,
            event.kind,
            event.message,
            event.payload,
            level,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Append a session event inside the caller's transaction. Runtime state
/// changes that are exposed through the event stream use this form so a
/// projection and the event describing it cannot commit independently.
pub fn append_session_event_in_tx(
    tx: &Transaction<'_>,
    event: &SessionEventRow,
) -> Result<i64, DbError> {
    validate_session_event(event)?;
    let level = session_event_level(event);
    tx.execute(
        "INSERT INTO session_events (occurred_at, info_hash, kind, message, payload, level)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.occurred_at,
            event.info_hash,
            event.kind,
            event.message,
            event.payload,
            level,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn prune_session_events(conn: &Connection, retention: usize) -> Result<usize, DbError> {
    let deleted = conn.execute(
        "DELETE FROM session_events
         WHERE event_id NOT IN (
             SELECT event_id FROM session_events ORDER BY event_id DESC LIMIT ?1
         )",
        params![sqlite_limit(retention)],
    )?;
    Ok(deleted)
}

/// Prune session events inside the same transaction as a newly appended
/// event. Retention maintenance must not create a second commit boundary for
/// an otherwise atomic projection update.
pub fn prune_session_events_in_tx(
    tx: &Transaction<'_>,
    retention: usize,
) -> Result<usize, DbError> {
    let deleted = tx.execute(
        "DELETE FROM session_events
         WHERE event_id NOT IN (
             SELECT event_id FROM session_events ORDER BY event_id DESC LIMIT ?1
         )",
        params![sqlite_limit(retention)],
    )?;
    Ok(deleted)
}

pub fn list_session_events(
    conn: &Connection,
    info_hash: Option<&str>,
    limit: usize,
) -> Result<Vec<SessionEventRow>, DbError> {
    list_session_events_filtered(conn, info_hash, None, &[], None, limit)
}

pub fn list_session_events_filtered(
    conn: &Connection,
    info_hash: Option<&str>,
    kind: Option<&str>,
    levels: &[String],
    last_known_id: Option<i64>,
    limit: usize,
) -> Result<Vec<SessionEventRow>, DbError> {
    if levels.len() > MAX_EVENT_LEVEL_FILTER_ITEMS {
        return Err(DbError::ValueTooLarge {
            field: "session event level filter",
            len: levels.len() as u64,
            max: MAX_EVENT_LEVEL_FILTER_ITEMS as u64,
        });
    }
    let limit = sqlite_limit(limit.min(MAX_SESSION_EVENT_RESULT_ITEMS));
    let mut sql = String::from(
        "SELECT event_id, occurred_at, info_hash, kind, message, payload FROM session_events",
    );
    let mut clauses = Vec::new();
    let mut values = Vec::<Value>::new();
    if let Some(info_hash) = info_hash {
        clauses.push("info_hash = ?");
        values.push(Value::Text(info_hash.to_owned()));
    }
    if let Some(kind) = kind {
        clauses.push("kind = ?");
        values.push(Value::Text(kind.to_owned()));
    }
    if let Some(last_known_id) = last_known_id {
        clauses.push("event_id > ?");
        values.push(Value::Integer(last_known_id));
    }
    let level_placeholders = if levels.is_empty() {
        String::new()
    } else {
        std::iter::repeat_n("?", levels.len())
            .collect::<Vec<_>>()
            .join(",")
    };
    let level_clause;
    if !levels.is_empty() {
        level_clause = format!("lower(level) IN ({level_placeholders})");
        clauses.push(&level_clause);
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY event_id DESC LIMIT ?");
    for level in levels {
        values.push(Value::Text(canonical_level(level)));
    }
    values.push(Value::Integer(limit));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(values), SessionEventRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn session_event_level(row: &SessionEventRow) -> &'static str {
    let payload_level = serde_json::from_str::<serde_json::Value>(&row.payload)
        .ok()
        .and_then(|value| {
            value
                .get("level")
                .and_then(|level| level.as_str())
                .map(str::to_ascii_lowercase)
        });
    match payload_level.as_deref() {
        Some("error") | Some("critical") => "error",
        Some("warn") | Some("warning") => "warn",
        Some("info") => "info",
        _ => level_from_kind(&row.kind),
    }
}

fn canonical_level(level: &str) -> String {
    match level.to_ascii_lowercase().as_str() {
        "error" | "critical" => "error".to_owned(),
        "warn" | "warning" => "warn".to_owned(),
        "info" => "info".to_owned(),
        other => other.to_owned(),
    }
}

fn level_from_kind(kind: &str) -> &'static str {
    let lower = kind.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") {
        "error"
    } else if lower.contains("warn") {
        "warn"
    } else {
        "info"
    }
}

pub fn append_job_event(conn: &Connection, event: &JobEventRow) -> Result<i64, DbError> {
    validate_job_event(event)?;
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

/// Append a job event inside a caller-owned transaction so it can commit with
/// the job projection it describes.
pub fn append_job_event_in_tx(tx: &Transaction<'_>, event: &JobEventRow) -> Result<i64, DbError> {
    validate_job_event(event)?;
    tx.execute(
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
    Ok(tx.last_insert_rowid())
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
        .query_map(
            params![job_id, sqlite_limit(limit.min(MAX_JOB_EVENT_RESULT_ITEMS)),],
            JobEventRow::from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Returns the oldest event for a job. Recovery uses this to retrieve the
/// original storage-plan context without loading an unbounded event history;
/// the newest checkpoint and the oldest queued event are the only records it
/// needs to reconstruct a plan.
pub fn first_job_event(conn: &Connection, job_id: &str) -> Result<Option<JobEventRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT event_id, job_id, occurred_at, kind, message, payload
         FROM job_events
         WHERE job_id = ?1
         ORDER BY event_id ASC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![job_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(JobEventRow::from_row(row)?)),
        None => Ok(None),
    }
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
    fn append_session_event_rejects_invalid_json_payload() {
        let conn = setup();
        let error = append_session_event(
            &conn,
            &SessionEventRow {
                event_id: None,
                occurred_at: 10,
                info_hash: None,
                kind: "corrupt".into(),
                message: None,
                payload: "{not-json}".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, DbError::Json(_)));
    }

    #[test]
    fn event_writes_reject_oversized_payloads() {
        let conn = setup();
        let session_payload =
            serde_json::to_string(&"x".repeat(MAX_SESSION_EVENT_PAYLOAD_BYTES)).unwrap();
        let error = append_session_event(
            &conn,
            &SessionEventRow {
                event_id: None,
                occurred_at: 10,
                info_hash: None,
                kind: "oversized".into(),
                message: None,
                payload: session_payload,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DbError::ValueTooLarge {
                field: "session event payload",
                ..
            }
        ));

        let job_payload = serde_json::to_string(&"x".repeat(MAX_JOB_EVENT_PAYLOAD_BYTES)).unwrap();
        let error = append_job_event(
            &conn,
            &JobEventRow {
                event_id: None,
                job_id: "job-1".into(),
                occurred_at: 10,
                kind: "oversized".into(),
                message: None,
                payload: job_payload,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DbError::ValueTooLarge {
                field: "job event payload",
                ..
            }
        ));
    }

    #[test]
    fn append_job_event_rejects_invalid_json_payload() {
        let conn = setup();
        let error = append_job_event(
            &conn,
            &JobEventRow {
                event_id: None,
                job_id: "job-1".into(),
                occurred_at: 10,
                kind: "corrupt".into(),
                message: None,
                payload: "{not-json}".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, DbError::Json(_)));
    }

    #[test]
    fn event_reads_reject_oversized_payloads_before_materializing_them() {
        let conn = setup();
        let session_payload =
            serde_json::to_string(&"x".repeat(MAX_SESSION_EVENT_PAYLOAD_BYTES)).unwrap();
        conn.execute(
            "INSERT INTO session_events
             (occurred_at, info_hash, kind, message, payload, level)
             VALUES (10, NULL, 'corrupt', NULL, ?1, 'info')",
            params![session_payload],
        )
        .unwrap();
        assert!(list_session_events(&conn, None, 1).is_err());

        job_row::upsert_job(
            &conn,
            &job_row::JobRow {
                job_id: "job-1".into(),
                kind: "test".into(),
                state: "queued".into(),
                dry_run: false,
                affected_torrents: Vec::new(),
                total: 0,
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
            },
        )
        .unwrap();
        let job_payload = serde_json::to_string(&"x".repeat(MAX_JOB_EVENT_PAYLOAD_BYTES)).unwrap();
        conn.execute(
            "INSERT INTO job_events (job_id, occurred_at, kind, message, payload)
             VALUES ('job-1', 10, 'corrupt', NULL, ?1)",
            params![job_payload],
        )
        .unwrap();
        assert!(list_job_events(&conn, "job-1", 1).is_err());
        assert!(first_job_event(&conn, "job-1").is_err());
    }

    #[test]
    fn list_session_events_filters_before_limit() {
        let conn = setup();
        append_session_event(
            &conn,
            &SessionEventRow {
                event_id: None,
                occurred_at: 10,
                info_hash: Some("a".repeat(40)),
                kind: "tracker_warning".into(),
                message: Some("warn".into()),
                payload: r#"{"level":"warn"}"#.into(),
            },
        )
        .unwrap();
        append_session_event(
            &conn,
            &SessionEventRow {
                event_id: None,
                occurred_at: 11,
                info_hash: Some("a".repeat(40)),
                kind: "torrent_added".into(),
                message: Some("info".into()),
                payload: r#"{"level":"info"}"#.into(),
            },
        )
        .unwrap();

        let events = list_session_events_filtered(
            &conn,
            Some(&"a".repeat(40)),
            None,
            &["warn".into()],
            None,
            1,
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "tracker_warning");
        let stored_level: String = conn
            .query_row(
                "SELECT level FROM session_events WHERE kind = 'tracker_warning'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_level, "warn");

        let events = list_session_events_filtered(&conn, None, None, &[], Some(1), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].event_id.unwrap_or_default() > 1);
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
        assert_eq!(
            first_job_event(&conn, "job-1").unwrap(),
            Some(events[0].clone())
        );
    }

    #[test]
    fn sqlite_limit_clamps_zero_and_large_values() {
        assert_eq!(sqlite_limit(0), 1);
        #[cfg(target_pointer_width = "64")]
        assert_eq!(sqlite_limit(i64::MAX as usize + 1), i64::MAX);
    }
}
