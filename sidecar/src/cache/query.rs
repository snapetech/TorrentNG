use anyhow::Result;
use rusqlite::{params, params_from_iter};
use serde::Deserialize;

use super::db::{Db, TorrentRow};

#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackerHealthRow {
    pub tracker: String,
    pub torrent_count: i64,
    pub active_count: i64,
    pub complete_count: i64,
    pub error_count: i64,
    pub seed_count: i64,
    pub peer_count: i64,
    pub last_updated: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SidebarFacets {
    pub status: std::collections::BTreeMap<String, i64>,
    pub media_type: std::collections::BTreeMap<String, i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub filter: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub tracker: Option<String>,
    pub media_type: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl Db {
    pub fn get(&self, hash: &str) -> Result<Option<TorrentRow>> {
        let conn = self.0.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT t.hash, t.name, t.size_bytes, t.bytes_done, t.down_rate, t.up_rate,
                    t.up_total, t.down_total, t.ratio, t.is_active, t.is_open, t.complete,
                    t.state, t.priority, t.category, t.base_path, t.directory, t.creation_date,
                    t.timestamp_finished, t.tracker_focus, t.peers_connected, t.peers_complete,
                    t.message, t.tracker_url,
                    COALESCE(tags.tags, '') AS tags,
                    t.updated_at
             FROM torrents t
             LEFT JOIN (
                SELECT hash, GROUP_CONCAT(tag) AS tags
                FROM torrent_tags
                GROUP BY hash
             ) tags ON tags.hash=t.hash
             WHERE t.hash=?1 COLLATE NOCASE",
        )?;
        let mut rows = stmt.query(params![hash])?;
        match rows.next()? {
            None => Ok(None),
            Some(r) => Ok(Some(TorrentRow {
                hash: r.get(0)?,
                name: r.get(1)?,
                size_bytes: r.get(2)?,
                bytes_done: r.get(3)?,
                down_rate: r.get(4)?,
                up_rate: r.get(5)?,
                up_total: r.get(6)?,
                down_total: r.get(7)?,
                ratio: r.get(8)?,
                is_active: r.get::<_, i64>(9)? != 0,
                is_open: r.get::<_, i64>(10)? != 0,
                complete: r.get::<_, i64>(11)? != 0,
                state: r.get(12)?,
                priority: r.get(13)?,
                category: r.get(14)?,
                base_path: r.get(15)?,
                directory: r.get(16)?,
                creation_date: r.get(17)?,
                timestamp_finished: r.get(18)?,
                tracker_focus: r.get(19)?,
                peers_connected: r.get(20)?,
                peers_complete: r.get(21)?,
                message: r.get(22)?,
                tracker_url: r.get(23)?,
                tags: r.get(24)?,
                updated_at: r.get(25)?,
            })),
        }
    }

    /// Returns all torrents with `updated_at > since`, plus the current max `updated_at`.
    pub fn list_since(&self, since: i64) -> Result<(Vec<TorrentRow>, i64)> {
        let conn = self.0.lock().expect("db mutex");
        let max_updated_at: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(updated_at), 0) FROM torrents",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let sql = "SELECT t.hash, t.name, t.size_bytes, t.bytes_done, t.down_rate, t.up_rate,
                    t.up_total, t.down_total, t.ratio, t.is_active, t.is_open, t.complete,
                    t.state, t.priority, t.category, t.base_path, t.directory, t.creation_date,
                    t.timestamp_finished, t.tracker_focus, t.peers_connected, t.peers_complete,
                    t.message, t.tracker_url,
                    COALESCE(tags.tags, '') AS tags,
                    t.updated_at
             FROM torrents t
             LEFT JOIN (
                SELECT hash, GROUP_CONCAT(tag) AS tags
                FROM torrent_tags
                GROUP BY hash
             ) tags ON tags.hash=t.hash
             WHERE t.updated_at > ?1";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params![since], |r: &rusqlite::Row<'_>| {
                Ok(TorrentRow {
                    hash: r.get(0)?,
                    name: r.get(1)?,
                    size_bytes: r.get(2)?,
                    bytes_done: r.get(3)?,
                    down_rate: r.get(4)?,
                    up_rate: r.get(5)?,
                    up_total: r.get(6)?,
                    down_total: r.get(7)?,
                    ratio: r.get(8)?,
                    is_active: r.get::<_, i64>(9)? != 0,
                    is_open: r.get::<_, i64>(10)? != 0,
                    complete: r.get::<_, i64>(11)? != 0,
                    state: r.get(12)?,
                    priority: r.get(13)?,
                    category: r.get(14)?,
                    base_path: r.get(15)?,
                    directory: r.get(16)?,
                    creation_date: r.get(17)?,
                    timestamp_finished: r.get(18)?,
                    tracker_focus: r.get(19)?,
                    peers_connected: r.get(20)?,
                    peers_complete: r.get(21)?,
                    message: r.get(22)?,
                    tracker_url: r.get(23)?,
                    tags: r.get(24)?,
                    updated_at: r.get(25)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rows, max_updated_at.max(since)))
    }

    pub fn list(&self, p: &ListParams) -> Result<(Vec<TorrentRow>, i64)> {
        let (where_sql, args) = build_where(p);
        let order = order_clause(p.sort.as_deref(), p.dir.as_deref());
        let limit = p.limit.unwrap_or(200).clamp(1, 50000);
        let offset = p.offset.unwrap_or(0).max(0);

        let conn = self.0.lock().expect("db mutex");

        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM torrents t{where_sql}"),
            params_from_iter(args.iter()),
            |r: &rusqlite::Row<'_>| r.get(0),
        )?;

        let sql = format!(
            "SELECT t.hash, t.name, t.size_bytes, t.bytes_done, t.down_rate, t.up_rate,
                    t.up_total, t.down_total, t.ratio, t.is_active, t.is_open, t.complete,
                    t.state, t.priority, t.category, t.base_path, t.directory, t.creation_date,
                    t.timestamp_finished, t.tracker_focus, t.peers_connected, t.peers_complete,
                    t.message, t.tracker_url,
                    COALESCE(tags.tags, '') AS tags,
                    t.updated_at
             FROM torrents t
             LEFT JOIN (
                SELECT hash, GROUP_CONCAT(tag) AS tags
                FROM torrent_tags
                GROUP BY hash
             ) tags ON tags.hash=t.hash
             {where_sql} ORDER BY {order} LIMIT ?{n1} OFFSET ?{n2}",
            n1 = args.len() + 1,
            n2 = args.len() + 2,
        );

        let mut all_args: Vec<String> = args;
        all_args.push(limit.to_string());
        all_args.push(offset.to_string());

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params_from_iter(all_args.iter()),
                |r: &rusqlite::Row<'_>| {
                    Ok(TorrentRow {
                        hash: r.get(0)?,
                        name: r.get(1)?,
                        size_bytes: r.get(2)?,
                        bytes_done: r.get(3)?,
                        down_rate: r.get(4)?,
                        up_rate: r.get(5)?,
                        up_total: r.get(6)?,
                        down_total: r.get(7)?,
                        ratio: r.get(8)?,
                        is_active: r.get::<_, i64>(9)? != 0,
                        is_open: r.get::<_, i64>(10)? != 0,
                        complete: r.get::<_, i64>(11)? != 0,
                        state: r.get(12)?,
                        priority: r.get(13)?,
                        category: r.get(14)?,
                        base_path: r.get(15)?,
                        directory: r.get(16)?,
                        creation_date: r.get(17)?,
                        timestamp_finished: r.get(18)?,
                        tracker_focus: r.get(19)?,
                        peers_connected: r.get(20)?,
                        peers_complete: r.get(21)?,
                        message: r.get(22)?,
                        tracker_url: r.get(23)?,
                        tags: r.get(24)?,
                        updated_at: r.get(25)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((rows, total))
    }

    pub fn tracker_health(&self) -> Result<Vec<TrackerHealthRow>> {
        let conn = self.0.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT tracker_url,
                    COUNT(*) AS torrent_count,
                    SUM(CASE WHEN is_active != 0 THEN 1 ELSE 0 END) AS active_count,
                    SUM(CASE WHEN complete != 0 THEN 1 ELSE 0 END) AS complete_count,
                    SUM(CASE WHEN message != '' THEN 1 ELSE 0 END) AS error_count,
                    SUM(peers_complete) AS seed_count,
                    SUM(peers_connected) AS peer_count,
                    MAX(updated_at) AS last_updated
             FROM torrents
             WHERE tracker_url != ''
             GROUP BY tracker_url
             ORDER BY error_count DESC, torrent_count DESC, tracker_url COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TrackerHealthRow {
                    tracker: r.get(0)?,
                    torrent_count: r.get(1)?,
                    active_count: r.get(2)?,
                    complete_count: r.get(3)?,
                    error_count: r.get(4)?,
                    seed_count: r.get(5)?,
                    peer_count: r.get(6)?,
                    last_updated: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn sidebar_facets(&self) -> Result<SidebarFacets> {
        let conn = self.0.lock().expect("db mutex");
        let mut status = std::collections::BTreeMap::new();

        let status_queries = [
            ("all", "1=1"),
            ("downloading", "complete=0 AND is_active=1"),
            ("seeding", "complete=1 AND is_active=1"),
            ("completed", "complete=1"),
            ("running", "is_open=1"),
            ("queued", "state=1 AND is_active=0"),
            ("stopped", "state=0 AND is_active=0"),
            ("active", "is_active=1"),
            ("inactive", "is_active=0"),
            ("stalled", "is_open=1 AND is_active=0"),
            (
                "stalled_uploading",
                "complete=1 AND is_open=1 AND is_active=0",
            ),
            (
                "stalled_downloading",
                "complete=0 AND is_open=1 AND is_active=0",
            ),
            ("checking", "state=2"),
            ("moving", "0=1"),
            ("error", "message != '' AND is_active=0"),
        ];
        for (key, where_sql) in status_queries {
            let count: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM torrents WHERE {where_sql}"),
                [],
                |r| r.get(0),
            )?;
            status.insert(key.to_owned(), count);
        }

        let mut media_type = std::collections::BTreeMap::new();
        let media_types = ["ebook", "tv", "video", "audio", "image", "game", "software"];
        for key in media_types {
            let mut clauses = Vec::new();
            let mut args = Vec::new();
            append_media_type_clause(key, &mut clauses, &mut args);
            let where_sql = if clauses.is_empty() {
                "1=0".to_owned()
            } else {
                clauses.join(" AND ")
            };
            let count: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM torrents t WHERE {where_sql}"),
                params_from_iter(args.iter()),
                |r| r.get(0),
            )?;
            media_type.insert(key.to_owned(), count);
        }

        Ok(SidebarFacets { status, media_type })
    }
}

fn build_where(p: &ListParams) -> (String, Vec<String>) {
    let mut clauses = Vec::new();
    let mut args = Vec::new();

    if let Some(f) = &p.filter {
        if !f.is_empty() {
            clauses.push(format!("t.name LIKE ?{} COLLATE NOCASE", args.len() + 1));
            args.push(format!("%{f}%"));
        }
    }
    if let Some(cat) = &p.category {
        clauses.push(format!("t.category = ?{}", args.len() + 1));
        args.push(cat.clone());
    }
    if let Some(tag) = &p.tag {
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM torrent_tags tt WHERE tt.hash=t.hash AND tt.tag=?{})",
            args.len() + 1
        ));
        args.push(tag.clone());
    }
    if let Some(tracker) = &p.tracker {
        if !tracker.is_empty() {
            clauses.push(format!(
                "t.tracker_url LIKE ?{} COLLATE NOCASE",
                args.len() + 1
            ));
            args.push(format!("%{tracker}%"));
        }
    }
    if let Some(media_type) = &p.media_type {
        append_media_type_clause(media_type, &mut clauses, &mut args);
    }
    if let Some(status) = &p.status {
        match status.as_str() {
            "running" => clauses.push("t.is_open=1".into()),
            "seeding" => clauses.push("t.complete=1 AND t.is_active=1".into()),
            "downloading" => clauses.push("t.complete=0 AND t.is_active=1".into()),
            "completed" => clauses.push("t.complete=1".into()),
            "active" => clauses.push("t.is_active=1".into()),
            "inactive" => clauses.push("t.is_active=0".into()),
            "queued" => clauses.push("t.state=1 AND t.is_active=0".into()),
            "paused" | "stopped" => clauses.push("t.state=0 AND t.is_active=0".into()),
            "stalled" => clauses.push("t.is_open=1 AND t.is_active=0".into()),
            "stalled_uploading" => {
                clauses.push("t.complete=1 AND t.is_open=1 AND t.is_active=0".into())
            }
            "stalled_downloading" => {
                clauses.push("t.complete=0 AND t.is_open=1 AND t.is_active=0".into())
            }
            "checking" => clauses.push("t.state=2".into()),
            "moving" => clauses.push("0=1".into()),
            "error" | "errored" => clauses.push("t.message != '' AND t.is_active=0".into()),
            _ => {}
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    (where_sql, args)
}

fn append_media_type_clause(media_type: &str, clauses: &mut Vec<String>, args: &mut Vec<String>) {
    let patterns: &[&str] = match media_type {
        "ebook" => &[
            "ebook",
            "ebooks",
            "book",
            "books",
            "audiobook",
            ".epub",
            ".mobi",
            ".azw3",
            ".pdf",
            ".cbz",
            ".cbr",
        ],
        "tv" => &[
            "s%e%", "season", "episode", "hdtv", "web-dl", "webrip", "tv",
        ],
        "video" => &[
            "movie", "movies", "film", "bluray", "bdrip", "dvdrip", "x264", "x265", "2160p",
            "1080p", "720p", ".mkv", ".mp4", ".avi", ".mov", ".wmv", ".m4v",
        ],
        "audio" => &[
            "music",
            "album",
            "discography",
            ".flac",
            ".mp3",
            ".aac",
            ".ogg",
            ".opus",
            ".wav",
            ".m4a",
        ],
        "image" => &[
            ".iso",
            ".img",
            ".dmg",
            "installer",
            "image",
            "linux",
            "ubuntu",
            "debian",
            "fedora",
        ],
        "game" => &[
            "game", "games", "gog", "steam", "switch", "ps4", "ps5", "xbox",
        ],
        "software" => &[
            "app", "software", "source", "code", "github", "windows", "macos", ".exe", ".msi",
            ".pkg", ".deb", ".rpm", ".zip", ".tar", ".gz", ".xz", ".7z", ".rar",
        ],
        _ => &[],
    };
    if patterns.is_empty() {
        return;
    }

    let mut terms = Vec::new();
    for pattern in patterns {
        let idx = args.len() + 1;
        terms.push(format!(
            "(t.name LIKE ?{idx} COLLATE NOCASE OR t.category LIKE ?{idx} COLLATE NOCASE OR t.directory LIKE ?{idx} COLLATE NOCASE)"
        ));
        args.push(format!("%{pattern}%"));
    }
    clauses.push(format!("({})", terms.join(" OR ")));
}

fn order_clause(sort: Option<&str>, dir: Option<&str>) -> String {
    let col = match sort {
        Some("name") => "t.name COLLATE NOCASE",
        Some("size") => "t.size_bytes",
        Some("added") => "t.creation_date",
        Some("ratio") => "t.ratio",
        Some("speed_down") => "t.down_rate",
        Some("speed_up") => "t.up_rate",
        Some("progress") => "CAST(t.bytes_done AS REAL) / NULLIF(t.size_bytes, 0)",
        _ => "t.name COLLATE NOCASE",
    };
    let d = if dir.map(|d| d.eq_ignore_ascii_case("desc")).unwrap_or(false) {
        "DESC"
    } else {
        "ASC"
    };
    format!("{col} {d}")
}
