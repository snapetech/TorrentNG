use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::time::sleep;

use crate::{
    cache::{AppEventRow, Db},
    config::RtorrentLogConfig,
};

pub async fn run(db: Arc<Db>, config: RtorrentLogConfig, retention: usize) {
    if !config.enabled || config.paths.is_empty() {
        return;
    }
    let interval = Duration::from_secs(config.poll_interval_secs.max(1));

    loop {
        for path in &config.paths {
            if let Err(e) = ingest_path(&db, path, retention, config.read_from_start) {
                let source = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("rtorrent.log");
                tracing::warn!(
                    component = "rtorrent_logs",
                    operation = "ingest",
                    source,
                    error = %e,
                    "failed to ingest rtorrent log"
                );
            }
        }
        sleep(interval).await;
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
    file.read_to_end(&mut buf)?;
    let buf = String::from_utf8_lossy(&buf);

    for line in buf.lines().map(str::trim).filter(|line| !line.is_empty()) {
        append_log_line(db, path, line, retention)?;
    }
    db.set_kv(&offset_key, &metadata.len().to_string())?;
    Ok(())
}

fn offset_key(path: &Path) -> String {
    let stable_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    format!("rtorrent_log_offset:{}", stable_path.to_string_lossy())
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
}
