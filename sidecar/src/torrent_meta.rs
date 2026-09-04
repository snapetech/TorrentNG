use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};

const DEFAULT_SESSION_DIR: &str = "/session";
const MAX_BENCODE_DEPTH: usize = 64;
const MAX_SESSION_TORRENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRACKER_URL_BYTES: usize = 8 * 1024;
const MAX_TRACKER_URLS: usize = 1024;

pub fn session_tracker_url(hash: &str, cache: &mut HashMap<String, Option<String>>) -> String {
    let Ok(normalized) = normalize_hash(hash) else {
        return String::new();
    };
    if let Some(cached) = cache.get(&normalized) {
        return cached.clone().unwrap_or_default();
    }

    let tracker = read_session_torrent(&normalized)
        .ok()
        .and_then(|raw| first_tracker_url(&raw));
    cache.insert(normalized, tracker.clone());
    tracker.unwrap_or_default()
}

pub fn session_tracker_urls(hash: &str) -> Vec<String> {
    session_torrent_blob(hash)
        .ok()
        .map(|raw| tracker_urls_from_torrent(&raw))
        .unwrap_or_default()
}

pub fn session_torrent_blob(hash: &str) -> std::io::Result<Vec<u8>> {
    let normalized = normalize_hash(hash)?;
    read_session_torrent(&normalized)
}

fn normalize_hash(hash: &str) -> io::Result<String> {
    let normalized = hash.trim().to_ascii_uppercase();
    if normalized.is_empty()
        || normalized.len() > 64
        || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "torrent hash must be a non-empty hexadecimal filename component",
        ));
    }
    Ok(normalized)
}

fn read_session_torrent(hash: &str) -> io::Result<Vec<u8>> {
    let path = session_dir().join(format!("{hash}.torrent"));
    let file = open_read_no_follow(&path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_SESSION_TORRENT_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session torrent {} is {file_len} bytes, maximum is {MAX_SESSION_TORRENT_BYTES}",
                path.display()
            ),
        ));
    }
    let mut raw = Vec::with_capacity(
        usize::try_from(file_len)
            .unwrap_or(MAX_SESSION_TORRENT_BYTES)
            .min(MAX_SESSION_TORRENT_BYTES),
    );
    let mut limited = file.take(MAX_SESSION_TORRENT_BYTES.saturating_add(1) as u64);
    limited.read_to_end(&mut raw)?;
    if raw.len() > MAX_SESSION_TORRENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session torrent {} grew beyond the {MAX_SESSION_TORRENT_BYTES} byte limit",
                path.display()
            ),
        ));
    }
    Ok(raw)
}

#[cfg(unix)]
fn open_read_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn session_dir() -> PathBuf {
    std::env::var("TNG_SESSION_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SESSION_DIR))
}

fn first_tracker_url(raw: &[u8]) -> Option<String> {
    tracker_urls_from_torrent(raw).into_iter().next()
}

fn tracker_urls_from_torrent(raw: &[u8]) -> Vec<String> {
    let mut pos = 0;
    if raw.get(pos) != Some(&b'd') {
        return Vec::new();
    }
    pos += 1;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    while pos < raw.len() && raw[pos] != b'e' {
        let Some(key) = parse_bytes(raw, &mut pos) else {
            break;
        };
        if key == b"announce" {
            if let Some(value) = parse_bytes(raw, &mut pos).and_then(clean_tracker_url) {
                push_tracker(value, &mut seen, &mut out);
            }
        } else if key == b"announce-list" {
            collect_trackers_in_value(raw, &mut pos, 0, &mut seen, &mut out);
        } else {
            if skip_value(raw, &mut pos, 0).is_none() {
                break;
            }
        }
    }

    out
}

fn push_tracker(
    value: String,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
) {
    if out.len() >= MAX_TRACKER_URLS {
        return;
    }
    if seen.insert(value.clone()) {
        out.push(value);
    }
}

fn clean_tracker_url(raw: &[u8]) -> Option<String> {
    if raw.len() > MAX_TRACKER_URL_BYTES {
        return None;
    }
    let value = String::from_utf8_lossy(raw).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn collect_trackers_in_value(
    raw: &[u8],
    pos: &mut usize,
    depth: usize,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
) -> Option<()> {
    if depth > MAX_BENCODE_DEPTH || *pos >= raw.len() {
        return None;
    }
    match raw[*pos] {
        b'0'..=b'9' => {
            if let Some(url) = parse_bytes(raw, pos).and_then(clean_tracker_url) {
                push_tracker(url, seen, out);
            }
            Some(())
        }
        b'l' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                collect_trackers_in_value(raw, pos, depth + 1, seen, out)?;
            }
            if *pos >= raw.len() {
                return None;
            }
            *pos += 1;
            Some(())
        }
        b'd' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                parse_bytes(raw, pos)?;
                collect_trackers_in_value(raw, pos, depth + 1, seen, out)?;
            }
            if *pos >= raw.len() {
                return None;
            }
            *pos += 1;
            Some(())
        }
        b'i' => skip_value(raw, pos, depth),
        _ => None,
    }
}

fn parse_bytes<'a>(raw: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let start = *pos;
    while *pos < raw.len() && raw[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start || raw.get(*pos) != Some(&b':') {
        return None;
    }
    let len = std::str::from_utf8(&raw[start..*pos])
        .ok()?
        .parse::<usize>()
        .ok()?;
    *pos += 1;
    let end = pos.checked_add(len)?;
    if end > raw.len() {
        return None;
    }
    let out = &raw[*pos..end];
    *pos = end;
    Some(out)
}

fn skip_value(raw: &[u8], pos: &mut usize, depth: usize) -> Option<()> {
    if depth > MAX_BENCODE_DEPTH || *pos >= raw.len() {
        return None;
    }
    match raw[*pos] {
        b'0'..=b'9' => {
            parse_bytes(raw, pos)?;
            Some(())
        }
        b'i' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                *pos += 1;
            }
            if *pos >= raw.len() {
                return None;
            }
            *pos += 1;
            Some(())
        }
        b'l' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                skip_value(raw, pos, depth + 1)?;
            }
            if *pos >= raw.len() {
                return None;
            }
            *pos += 1;
            Some(())
        }
        b'd' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                parse_bytes(raw, pos)?;
                skip_value(raw, pos, depth + 1)?;
            }
            if *pos >= raw.len() {
                return None;
            }
            *pos += 1;
            Some(())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        first_tracker_url, session_torrent_blob, tracker_urls_from_torrent, MAX_TRACKER_URL_BYTES,
    };

    #[test]
    fn reads_announce_first() {
        let raw = b"d8:announce24:https://tracker/announce4:infod4:name4:teste";
        assert_eq!(
            first_tracker_url(raw).as_deref(),
            Some("https://tracker/announce")
        );
    }

    #[test]
    fn falls_back_to_announce_list() {
        let raw = b"d13:announce-listll27:udp://tracker:6969/announceee4:infod4:name4:teste";
        assert_eq!(
            first_tracker_url(raw).as_deref(),
            Some("udp://tracker:6969/announce")
        );
    }

    #[test]
    fn deduplicates_tracker_urls() {
        let raw = b"d8:announce24:https://tracker/announce13:announce-listll24:https://tracker/announceel27:udp://tracker:6969/announceeee";
        assert_eq!(
            tracker_urls_from_torrent(raw),
            vec![
                "https://tracker/announce".to_owned(),
                "udp://tracker:6969/announce".to_owned()
            ]
        );
    }

    #[test]
    fn rejects_path_like_hashes() {
        assert!(session_torrent_blob("../secret").is_err());
        assert!(session_torrent_blob("ABC/DEF").is_err());
    }

    #[test]
    fn ignores_oversized_tracker_urls() {
        let raw = format!(
            "d8:announce{}:{}e",
            MAX_TRACKER_URL_BYTES + 1,
            "x".repeat(MAX_TRACKER_URL_BYTES + 1)
        );
        assert!(first_tracker_url(raw.as_bytes()).is_none());
    }
}
