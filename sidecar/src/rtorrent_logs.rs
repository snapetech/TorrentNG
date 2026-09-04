use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::time::sleep;

use crate::{
    cache::{AppEventRow, Db},
    config::RtorrentLogConfig,
};

const MAX_LOG_BYTES_PER_POLL: u64 = 1024 * 1024;

pub async fn run(db: Arc<Db>, config: RtorrentLogConfig, retention: usize) {
    if !config.enabled || config.paths.is_empty() {
        return;
    }
    let interval = Duration::from_secs(config.poll_interval_secs.max(1));
    let paths = config.paths;
    let read_from_start = config.read_from_start;

    loop {
        let db_for_poll = Arc::clone(&db);
        let paths_for_poll = paths.clone();
        let poll = tokio::task::spawn_blocking(move || {
            poll_paths(&db_for_poll, &paths_for_poll, retention, read_from_start)
        })
        .await;
        if let Err(error) = poll {
            tracing::warn!(
                component = "rtorrent_logs",
                operation = "poll",
                result = "error",
                error = %error,
                "rTorrent log poll worker failed"
            );
        }
        sleep(interval).await;
    }
}

fn poll_paths(db: &Db, paths: &[PathBuf], retention: usize, read_from_start: bool) {
    for path in paths {
        match ingest_path(db, path, retention, read_from_start) {
            Ok(()) => {
                if let Err(e) = record_ingest_recovery(db, path, retention) {
                    tracing::warn!(
                        component = "rtorrent_logs",
                        operation = "record_recovery",
                        source = log_source(path),
                        result = "error",
                        error = %e,
                        "failed to record rtorrent log ingest recovery"
                    );
                }
            }
            Err(e) => {
                let source = log_source(path);
                tracing::warn!(
                    component = "rtorrent_logs",
                    operation = "ingest",
                    source,
                    result = "error",
                    error = %e,
                    "failed to ingest rtorrent log"
                );
                if let Err(event_error) = record_ingest_failure(db, path, &e, retention) {
                    tracing::warn!(
                        component = "rtorrent_logs",
                        operation = "record_failure",
                        source,
                        result = "error",
                        error = %event_error,
                        "failed to record rtorrent log ingest failure"
                    );
                }
            }
        }
    }
}

fn ingest_path(db: &Db, path: &Path, retention: usize, read_from_start: bool) -> Result<()> {
    let metadata = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let offset_key = offset_key(path);
    let mut offset = match db.get_kv(&offset_key)?.and_then(|value| value.parse().ok()) {
        Some(offset) => offset,
        None if read_from_start => 0,
        None => {
            db.set_kv(&offset_key, &metadata.len().to_string())?;
            return Ok(());
        }
    };
    if metadata.len() < offset {
        offset = 0;
    }
    if metadata.len() == offset {
        return Ok(());
    }

    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.take(MAX_LOG_BYTES_PER_POLL).read_to_end(&mut buf)?;

    // Advance only through complete lines. A partial trailing line is left
    // for the next poll so rotation or an interrupted write cannot silently
    // discard its prefix. If one line itself exceeds the per-poll cap, emit
    // the bounded chunk and advance so a pathological log cannot pin this
    // loop forever rereading the same bytes.
    let complete_bytes = buf
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .filter(|end| *end > 0 || buf.len() < MAX_LOG_BYTES_PER_POLL as usize)
        .unwrap_or({
            if buf.len() == MAX_LOG_BYTES_PER_POLL as usize {
                buf.len()
            } else {
                0
            }
        });
    let text = String::from_utf8_lossy(&buf[..complete_bytes]);

    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        append_log_line(db, path, line, retention)?;
    }
    let next_offset = offset.saturating_add(complete_bytes as u64);
    db.set_kv(&offset_key, &next_offset.to_string())?;
    Ok(())
}

fn offset_key(path: &Path) -> String {
    let stable_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("rtorrent_log_offset:{}", stable_path.to_string_lossy())
}

fn error_key(path: &Path) -> String {
    let stable_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("rtorrent_log_error:{}", stable_path.to_string_lossy())
}

fn log_source(path: &Path) -> &str {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("rtorrent.log")
}

fn record_ingest_failure(
    db: &Db,
    path: &Path,
    error: &anyhow::Error,
    retention: usize,
) -> Result<()> {
    let key = error_key(path);
    let redacted_error = redact_log_line(&error.to_string());
    if db.get_kv(&key)?.as_deref() == Some(redacted_error.as_str()) {
        return Ok(());
    }
    db.set_kv(&key, &redacted_error)?;
    let source = log_source(path);
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: "warn".to_owned(),
            kind: "rtorrent_log_ingest_error".to_owned(),
            message: format!("rTorrent log ingest failed for {source}: {redacted_error}"),
            payload: serde_json::json!({
                "component": "rtorrent_logs",
                "operation": "ingest",
                "source": source,
                "result": "error",
                "error": redacted_error,
            })
            .to_string(),
        },
        retention,
    )?;
    Ok(())
}

fn record_ingest_recovery(db: &Db, path: &Path, retention: usize) -> Result<()> {
    let key = error_key(path);
    if db.get_kv(&key)?.is_none() {
        return Ok(());
    }
    db.delete_kv(&key)?;
    let source = log_source(path);
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: "info".to_owned(),
            kind: "rtorrent_log_ingest_recovered".to_owned(),
            message: format!("rTorrent log ingest recovered for {source}"),
            payload: serde_json::json!({
                "component": "rtorrent_logs",
                "operation": "ingest",
                "source": source,
                "result": "recovered",
            })
            .to_string(),
        },
        retention,
    )?;
    Ok(())
}

fn append_log_line(db: &Db, path: &Path, line: &str, retention: usize) -> Result<()> {
    let redacted = redact_log_line(line);
    let level = classify_level(&redacted);
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: chrono::Utc::now().timestamp(),
            level: level.to_owned(),
            kind: "rtorrent_log".to_owned(),
            message: redacted.clone(),
            payload: serde_json::json!({
                "component": "rtorrent",
                "operation": "log_ingest",
                "source": path.file_name().and_then(|s| s.to_str()).unwrap_or("rtorrent.log"),
                "level": level,
            })
            .to_string(),
        },
        retention,
    )?;
    Ok(())
}

fn classify_level(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();
    if lower.contains("critical") || lower.contains("fatal") || lower.contains("error") {
        "error"
    } else if lower.contains("warn") || lower.contains("failed") || lower.contains("failure") {
        "warn"
    } else {
        "info"
    }
}

fn redact_log_line(line: &str) -> String {
    line.split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        return "[redacted-magnet]".to_owned();
    }
    if lower.contains("passkey=")
        || lower.contains("apikey=")
        || lower.contains("api_key=")
        || lower.contains("token=")
        || lower.contains("cookie=")
    {
        return redact_query_token(token);
    }
    if looks_like_path(token) {
        return redact_path_token(token);
    }
    token.to_owned()
}

fn redact_query_token(token: &str) -> String {
    let separators = ['&', ';'];
    let mut out = token.to_owned();
    for key in ["passkey", "apikey", "api_key", "token", "cookie"] {
        let needle = format!("{key}=");
        if let Some(pos) = out.to_ascii_lowercase().find(&needle) {
            let value_start = pos + needle.len();
            let value_end = out[value_start..]
                .find(separators)
                .map(|idx| value_start + idx)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, "[redacted]");
        }
    }
    out
}

fn looks_like_path(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
    trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
}

fn redact_path_token(token: &str) -> String {
    let trimmed = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
    let suffix = Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("path");
    token.replace(trimmed, &format!("[redacted-path:{suffix}]"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn redacts_magnets_paths_and_secret_query_values() {
        let line =
            r#"loaded /data/private/movie.mkv magnet:?xt=urn:btih:abc tracker?passkey=secret&x=1"#;
        let redacted = redact_log_line(line);
        assert!(!redacted.contains("/data/private"));
        assert!(!redacted.contains("magnet:?"));
        assert!(!redacted.contains("secret"));
        assert!(redacted.contains("[redacted-path:movie.mkv]"));
        assert!(redacted.contains("passkey=[redacted]"));

        let redacted = redact_log_line("tracker?passkey=secret;token=also-secret&x=1");
        assert!(!redacted.contains("secret"));
        assert!(redacted.contains("passkey=[redacted];token=[redacted]&x=1"));
    }

    #[test]
    fn classifies_common_rtorrent_lines() {
        assert_eq!(classify_level("Could not open file: error"), "error");
        assert_eq!(classify_level("tracker warning"), "warn");
        assert_eq!(classify_level("download inserted"), "info");
    }

    #[test]
    fn ingest_path_appends_new_lines_and_handles_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("rtorrent.log");
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        std::fs::write(&log_path, "first line\n").unwrap();

        ingest_path(&db, &log_path, 10, false).unwrap();
        assert!(db.list_app_events(10).unwrap().is_empty());

        std::fs::write(
            &log_path,
            "first line\nsecond warning /tmp/secret/file.torrent\n",
        )
        .unwrap();
        ingest_path(&db, &log_path, 10, false).unwrap();
        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].level, "warn");
        assert!(!events[0].message.contains("/tmp/secret"));

        std::fs::write(&log_path, "rotated error\n").unwrap();
        ingest_path(&db, &log_path, 10, false).unwrap();
        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].level, "error");
    }

    #[test]
    fn ingest_path_can_import_existing_file_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("rtorrent.log");
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        std::fs::write(&log_path, "existing line\n").unwrap();

        ingest_path(&db, &log_path, 10, true).unwrap();

        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message, "existing line");
    }

    #[test]
    fn ingest_path_limits_each_poll_and_retries_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("rtorrent.log");
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut body = b"first\n".to_vec();
        body.extend(std::iter::repeat_n(b'x', MAX_LOG_BYTES_PER_POLL as usize));
        std::fs::write(&log_path, &body).unwrap();

        ingest_path(&db, &log_path, 10, true).unwrap();
        assert_eq!(db.list_app_events(10).unwrap().len(), 1);
        assert_eq!(
            db.get_kv(&offset_key(&log_path)).unwrap(),
            Some("6".to_owned())
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap()
            .write_all(b"\nsecond\n")
            .unwrap();
        ingest_path(&db, &log_path, 10, true).unwrap();
        ingest_path(&db, &log_path, 10, true).unwrap();

        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].message, "second");
        assert_eq!(events[1].message.len(), MAX_LOG_BYTES_PER_POLL as usize);
    }

    #[test]
    fn ingest_failures_are_durable_deduped_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("missing").join("rtorrent.log");
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let err = ingest_path(&db, &log_path, 10, false).unwrap_err();

        record_ingest_failure(&db, &log_path, &err, 10).unwrap();
        record_ingest_failure(&db, &log_path, &err, 10).unwrap();
        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "rtorrent_log_ingest_error");
        assert_eq!(events[0].level, "warn");
        assert!(!events[0]
            .message
            .contains(dir.path().to_string_lossy().as_ref()));

        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(&log_path, "ready\n").unwrap();
        ingest_path(&db, &log_path, 10, false).unwrap();
        record_ingest_recovery(&db, &log_path, 10).unwrap();

        let events = db.list_app_events(10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "rtorrent_log_ingest_recovered");
    }
}
