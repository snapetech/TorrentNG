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

fn session_dir() -> PathBuf {
    std::env::var("RTNG_SESSION_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SESSION_DIR))
}

fn first_tracker_url(raw: &[u8]) -> Option<String> {
    let mut pos = 0;
    if raw.get(pos) != Some(&b'd') {
        return None;
    }
    pos += 1;
    let mut announce_list: Option<String> = None;

    while pos < raw.len() && raw[pos] != b'e' {
        let key = parse_bytes(raw, &mut pos)?;
        if key == b"announce" {
            let value = parse_bytes(raw, &mut pos)?;
            if let Some(url) = clean_tracker_url(value) {
                return Some(url);
            }
        } else if key == b"announce-list" {
            announce_list = first_string_in_value(raw, &mut pos, 0).and_then(clean_tracker_url);
        } else {
            skip_value(raw, &mut pos, 0)?;
        }
    }

    announce_list
}

fn clean_tracker_url(raw: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(raw).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn first_string_in_value<'a>(raw: &'a [u8], pos: &mut usize, depth: usize) -> Option<&'a [u8]> {
    if depth > MAX_BENCODE_DEPTH || *pos >= raw.len() {
        return None;
    }
    match raw[*pos] {
        b'0'..=b'9' => parse_bytes(raw, pos),
        b'l' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                if let Some(value) = first_string_in_value(raw, pos, depth + 1) {
                    skip_remaining_list(raw, pos, depth + 1);
                    return Some(value);
                }
            }
            if *pos < raw.len() {
                *pos += 1;
            }
            None
        }
        b'd' => {
            *pos += 1;
            while *pos < raw.len() && raw[*pos] != b'e' {
                parse_bytes(raw, pos)?;
                if let Some(value) = first_string_in_value(raw, pos, depth + 1) {
                    skip_remaining_dict(raw, pos, depth + 1);
                    return Some(value);
                }
            }
            if *pos < raw.len() {
                *pos += 1;
            }
            None
        }
        b'i' => {
            skip_value(raw, pos, depth)?;
            None
        }
        _ => None,
    }
}

fn skip_remaining_list(raw: &[u8], pos: &mut usize, depth: usize) {
    while *pos < raw.len() && raw[*pos] != b'e' {
        if skip_value(raw, pos, depth).is_none() {
            return;
        }
    }
    if *pos < raw.len() {
        *pos += 1;
    }
}

fn skip_remaining_dict(raw: &[u8], pos: &mut usize, depth: usize) {
    while *pos < raw.len() && raw[*pos] != b'e' {
        if parse_bytes(raw, pos).is_none() || skip_value(raw, pos, depth).is_none() {
            return;
        }
    }
    if *pos < raw.len() {
        *pos += 1;
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
}
