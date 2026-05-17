use rusqlite::{params, params_from_iter, types::Value, Connection, Row};
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

pub fn prune_session_events(conn: &Connection, retention: usize) -> Result<usize, DbError> {
    let deleted = conn.execute(
        "DELETE FROM session_events
         WHERE event_id NOT IN (
             SELECT event_id FROM session_events ORDER BY event_id DESC LIMIT ?1
         )",
        params![retention.max(1) as i64],
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
    let limit = limit.max(1) as i64;
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
        std::iter::repeat("?")
            .take(levels.len())
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
    }
}
