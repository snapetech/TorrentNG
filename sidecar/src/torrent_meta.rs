use std::{collections::HashMap, path::PathBuf};

const DEFAULT_SESSION_DIR: &str = "/session";
const MAX_BENCODE_DEPTH: usize = 64;

pub fn session_tracker_url(hash: &str, cache: &mut HashMap<String, Option<String>>) -> String {
    let normalized = hash.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        return String::new();
    }
    if let Some(cached) = cache.get(&normalized) {
        return cached.clone().unwrap_or_default();
    }

    let path = session_dir().join(format!("{normalized}.torrent"));
    let tracker = std::fs::read(path)
        .ok()
        .and_then(|raw| first_tracker_url(&raw));
    cache.insert(normalized, tracker.clone());
    tracker.unwrap_or_default()
}

pub fn session_tracker_urls(hash: &str) -> Vec<String> {
    let normalized = hash.trim().to_ascii_uppercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    let path = session_dir().join(format!("{normalized}.torrent"));
    std::fs::read(path)
        .ok()
        .map(|raw| tracker_urls_from_torrent(&raw))
        .unwrap_or_default()
}

fn session_dir() -> PathBuf {
    std::env::var("RTNG_SESSION_DIR")
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
    if seen.insert(value.clone()) {
        out.push(value);
    }
}

fn clean_tracker_url(raw: &[u8]) -> Option<String> {
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
    use super::first_tracker_url;
    use super::tracker_urls_from_torrent;

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
}
