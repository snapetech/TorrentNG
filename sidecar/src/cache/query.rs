use anyhow::Result;
use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::Deserialize;

use super::db::{current_revision_locked, Db, TorrentRow, CACHE_REVISION_FLOOR_KEY};

/// Maximum rows materialized by a public compatible-client service page endpoint. Compatibility
/// protocols without paging use a separate, stricter whole-response policy.
pub const MAX_API_PAGE_ENTRIES: i64 = 5_000;
/// Prevent a client from turning SQL OFFSET into an arbitrary skip scan. The
    /// TorrentNG-client snapshot API is the path for deep, cursor-pinned exports.
pub const MAX_API_PAGE_OFFSET: i64 = 1_000_000;

// Keep cache filtering and ordering consistent with the qBittorrent wire
// projection in qbcompat::handlers::current_row_rates(). A rate older than
// this is no longer presented as live throughput.
const EFFECTIVE_DOWN_RATE_SQL: &str = "CASE WHEN t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 THEN 0 ELSE MAX(t.down_rate, 0) END";
const EFFECTIVE_UP_RATE_SQL: &str = "CASE WHEN t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 THEN 0 ELSE MAX(t.up_rate, 0) END";
const STALLED_SQL: &str = "t.is_active=1 AND ((t.complete=1 AND (t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 OR t.up_rate <= 0)) OR (t.complete=0 AND (t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 OR t.down_rate <= 0)))";
const STALLED_UPLOADING_SQL: &str = "t.complete=1 AND t.is_active=1 AND (t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 OR t.up_rate <= 0)";
const STALLED_DOWNLOADING_SQL: &str = "t.complete=0 AND t.is_active=1 AND (t.updated_at <= 0 OR CAST(strftime('%s', 'now') AS INTEGER) - t.updated_at > 15 OR t.down_rate <= 0)";
const TRACKER_ERROR_SQL: &str = "length(trim(t.message)) > 0";

#[derive(Debug)]
pub struct TorrentDelta {
    pub changed: Vec<TorrentRow>,
    pub removed: Vec<String>,
    pub revision: i64,
}

pub fn bounded_page_limit(limit: Option<i64>) -> Option<i64> {
    limit.map(|limit| limit.clamp(1, MAX_API_PAGE_ENTRIES))
}

pub fn validate_page_offset(offset: Option<i64>) -> Result<i64> {
    let offset = offset.unwrap_or(0);
    if !(0..=MAX_API_PAGE_OFFSET).contains(&offset) {
        anyhow::bail!("page offset must be between 0 and {MAX_API_PAGE_OFFSET}");
    }
    Ok(offset)
}

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

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone)]
pub struct TorrentLiveRow {
    pub hash: String,
    pub size_bytes: i64,
    pub bytes_done: i64,
    pub down_rate: i64,
    pub up_rate: i64,
    pub updated_at: i64,
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
                    COALESCE((
                        SELECT GROUP_CONCAT(ordered_tags.tag)
                        FROM (
                            SELECT tt.tag
                            FROM torrent_tags tt
                            WHERE tt.hash=t.hash
                            ORDER BY tt.tag COLLATE NOCASE, tt.tag
                        ) AS ordered_tags
                    ), '') AS tags,
                    t.updated_at
             FROM torrents t
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

    /// Read only the fields needed for visible-row ETA updates in one SQL
    /// statement instead of one query and connection lock per torrent.
    pub fn live_stats(&self, hashes: &[String]) -> Result<Vec<TorrentLiveRow>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=hashes.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT hash, size_bytes, bytes_done, down_rate, up_rate, updated_at
             FROM torrents
             WHERE hash COLLATE NOCASE IN ({placeholders})"
        );
        let conn = self.0.lock().expect("db mutex");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(hashes.iter()), |row| {
            Ok(TorrentLiveRow {
                hash: row.get(0)?,
                size_bytes: row.get(1)?,
                bytes_done: row.get(2)?,
                down_rate: row.get(3)?,
                up_rate: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns changed torrents and removals only when the delta fits within
    /// `max_rows` and the cursor is still replayable.
    ///
    /// qBittorrent's `sync/maindata` protocol has no cursor/page parameters
    /// for splitting a large delta. Returning `None` after reading one row
    /// past the bound, or when the tombstone history is too old, lets that
    /// caller reject the request rather than silently returning an incomplete
    /// projection. The cursor is a durable logical revision; wall-clock
    /// seconds are not used because multiple updates can happen in one second.
    pub fn list_since_bounded(&self, since: i64, max_rows: usize) -> Result<Option<TorrentDelta>> {
        if max_rows == 0 {
            return Ok(None);
        }
        let conn = self.0.lock().expect("db mutex");
        let revision = current_revision_locked(&conn)?;
        if since < 0 || since > revision {
            return Ok(None);
        }
        let floor: i64 = conn
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![CACHE_REVISION_FLOOR_KEY],
                |r| r.get(0),
            )
            .optional()?
            .map(|value: String| value.parse::<i64>())
            .transpose()
            .map_err(|error| anyhow::anyhow!("parse cache revision floor: {error}"))?
            .unwrap_or(0);
        if since < floor {
            return Ok(None);
        }

        let sql = "SELECT t.hash, t.name, t.size_bytes, t.bytes_done, t.down_rate, t.up_rate,
                    t.up_total, t.down_total, t.ratio, t.is_active, t.is_open, t.complete,
                    t.state, t.priority, t.category, t.base_path, t.directory, t.creation_date,
                    t.timestamp_finished, t.tracker_focus, t.peers_connected, t.peers_complete,
                    t.message, t.tracker_url,
                    COALESCE((
                        SELECT GROUP_CONCAT(ordered_tags.tag)
                        FROM (
                            SELECT tt.tag
                            FROM torrent_tags tt
                            WHERE tt.hash=t.hash
                            ORDER BY tt.tag COLLATE NOCASE, tt.tag
                        ) AS ordered_tags
                    ), '') AS tags,
                    t.updated_at, t.revision
             FROM torrents t
             WHERE t.revision > ?1
             ORDER BY t.revision ASC, t.hash COLLATE NOCASE ASC
             LIMIT ?2";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(
                params![since, max_rows.saturating_add(1) as i64],
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

        let mut removed_stmt = conn.prepare(
            "SELECT hash FROM removed_torrents
             WHERE revision > ?1
             ORDER BY revision ASC, hash COLLATE NOCASE ASC
             LIMIT ?2",
        )?;
        let removed = removed_stmt
            .query_map(params![since, max_rows.saturating_add(1) as i64], |row| {
                row.get(0)
            })?
            .collect::<rusqlite::Result<Vec<String>>>()?;

        if rows.len().saturating_add(removed.len()) > max_rows {
            return Ok(None);
        }
        Ok(Some(TorrentDelta {
            changed: rows,
            removed,
            revision,
        }))
    }

    pub fn list(&self, p: &ListParams) -> Result<(Vec<TorrentRow>, i64)> {
        let (where_sql, args) = build_where(p);
        let order = order_clause(p.sort.as_deref(), p.dir.as_deref());
        let limit = p.limit.unwrap_or(200).clamp(1, 50000);
        let offset = validate_page_offset(p.offset)?;

        let conn = self.0.lock().expect("db mutex");

        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM torrents t{where_sql}"),
            params_from_iter(args.iter()),
            |r: &rusqlite::Row<'_>| r.get(0),
        )?;

        let rows = query_torrent_rows(&conn, &where_sql, &args, &order, limit, offset)?;
        Ok((rows, total))
    }

    /// Fetch one page without calculating the result-set total. Compatibility
    /// endpoints such as qBittorrent's `torrents/info` do not expose a total;
    /// issuing a second filtered `COUNT(*)` query there is pure database work
    /// with no protocol value, especially for broad status filters.
    pub fn list_page(&self, p: &ListParams) -> Result<Vec<TorrentRow>> {
        let (where_sql, args) = build_where(p);
        let order = order_clause(p.sort.as_deref(), p.dir.as_deref());
        let limit = p.limit.unwrap_or(200).clamp(1, 50000);
        let offset = validate_page_offset(p.offset)?;
        let conn = self.0.lock().expect("db mutex");
        query_torrent_rows(&conn, &where_sql, &args, &order, limit, offset)
    }

    pub fn tracker_health(&self) -> Result<Vec<TrackerHealthRow>> {
        let conn = self.0.lock().expect("db mutex");
        let mut stmt = conn.prepare(
            "SELECT tracker_url,
                    COUNT(*) AS torrent_count,
                    SUM(CASE WHEN is_active != 0 THEN 1 ELSE 0 END) AS active_count,
                    SUM(CASE WHEN complete != 0 THEN 1 ELSE 0 END) AS complete_count,
                    SUM(CASE WHEN length(trim(message)) > 0 THEN 1 ELSE 0 END) AS error_count,
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

    /// Computes sidebar facet counts. `shared` supplies the free-text
    /// search plus category/tag/tracker filters (its own `status` and
    /// `media_type` fields are ignored) so counts stay in sync with an
    /// active search instead of always reflecting the whole library.
    pub fn sidebar_facets(&self, shared: &ListParams) -> Result<SidebarFacets> {
        let conn = self.0.lock().expect("db mutex");
        let (shared_clauses, shared_args) = shared_clauses(shared);
        let mut status = std::collections::BTreeMap::new();

        let status_queries = [
            ("all", "1=1"),
            ("downloading", "t.complete=0 AND t.is_active=1"),
            ("seeding", "t.complete=1 AND t.is_active=1"),
            ("completed", "t.complete=1"),
            ("running", "t.is_open=1"),
            // State 5 is the current queued projection. Keep accepting the
            // older state=1/is_open=0 shape from pre-migration cache rows.
            ("queued", "(t.state=5 OR (t.state=1 AND t.is_open=0))"),
            ("stopped", "t.state=0 AND t.is_active=0"),
            ("active", "t.is_active=1"),
            ("inactive", "t.is_active=0"),
            // "Stalled" means started but currently moving zero bytes (no
            // willing peers right now), NOT is_active=0 -- that's rTorrent's
            // d.is_active, which tracks started/stopped, not throughput. A
            // stopped torrent is "stopped", never "stalled".
            (
                "stalled",
                STALLED_SQL,
            ),
            (
                "stalled_uploading",
                STALLED_UPLOADING_SQL,
            ),
            (
                "stalled_downloading",
                STALLED_DOWNLOADING_SQL,
            ),
            ("checking", "t.state=2"),
            ("moving", "0=1"),
            ("error", "t.state=3"),
            // Distinct from the terminal state=3 "error" bucket above: a
            // torrent can be actively seeding/downloading just fine while
            // its tracker rejects announces (e.g. "torrent not registered
            // with this tracker"). This bucket is exactly
            // `tracker_health`'s existing error_count predicate (see
            // tracker_health() above), finally made filterable per-torrent,
            // not just visible as a per-tracker aggregate.
            ("tracker_error", TRACKER_ERROR_SQL),
        ];
        for (key, bucket_sql) in status_queries {
            let mut clauses = shared_clauses.clone();
            clauses.push(bucket_sql.to_owned());
            let where_sql = clauses.join(" AND ");
            let count: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM torrents t WHERE {where_sql}"),
                params_from_iter(shared_args.iter()),
                |r| r.get(0),
            )?;
            status.insert(key.to_owned(), count);
        }

        let mut media_type = std::collections::BTreeMap::new();
        for key in KNOWN_MEDIA_TYPES {
            let mut clauses = shared_clauses.clone();
            let mut args = shared_args.clone();
            append_media_type_clause(key, &mut clauses, &mut args);
            let where_sql = clauses.join(" AND ");
            let count: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM torrents t WHERE {where_sql}"),
                params_from_iter(args.iter()),
                |r| r.get(0),
            )?;
            media_type.insert((*key).to_owned(), count);
        }

        Ok(SidebarFacets { status, media_type })
    }
}

/// Clauses shared by both the main torrent list query and the sidebar facet
/// counts: free-text search plus category/tag/tracker filters. Deliberately
/// excludes `status` and `media_type` so each facet dimension can apply its
/// own bucket on top without filtering itself out of its own counts.
fn shared_clauses(p: &ListParams) -> (Vec<String>, Vec<String>) {
    let mut clauses = Vec::new();
    let mut args = Vec::new();

    if let Some(f) = &p.filter {
        if !f.is_empty() {
            let arg_index = args.len() + 1;
            clauses.push(format!(
                "(t.name LIKE ?{arg_index} ESCAPE char(92) COLLATE NOCASE
                  OR t.hash LIKE ?{arg_index} ESCAPE char(92) COLLATE NOCASE)"
            ));
            args.push(format!("%{}%", escape_like_pattern(f)));
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
                "instr(lower(t.tracker_url), lower(?{})) > 0",
                args.len() + 1
            ));
            args.push(tracker.clone());
        }
    }

    (clauses, args)
}

fn query_torrent_rows(
    conn: &rusqlite::Connection,
    where_sql: &str,
    args: &[String],
    order: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<TorrentRow>> {
    let sql = format!(
        "SELECT t.hash, t.name, t.size_bytes, t.bytes_done, t.down_rate, t.up_rate,
                t.up_total, t.down_total, t.ratio, t.is_active, t.is_open, t.complete,
                t.state, t.priority, t.category, t.base_path, t.directory, t.creation_date,
                t.timestamp_finished, t.tracker_focus, t.peers_connected, t.peers_complete,
                t.message, t.tracker_url,
                COALESCE((
                    SELECT GROUP_CONCAT(ordered_tags.tag)
                    FROM (
                        SELECT tt.tag
                        FROM torrent_tags tt
                        WHERE tt.hash=t.hash
                        ORDER BY tt.tag COLLATE NOCASE, tt.tag
                    ) AS ordered_tags
                ), '') AS tags,
                t.updated_at
         FROM torrents t
         {where_sql} ORDER BY {order} LIMIT ?{n1} OFFSET ?{n2}",
        n1 = args.len() + 1,
        n2 = args.len() + 2,
    );

    let mut all_args = args.to_vec();
    all_args.push(limit.to_string());
    all_args.push(offset.to_string());

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(all_args.iter()), torrent_row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn torrent_row_from_sql(r: &rusqlite::Row<'_>) -> rusqlite::Result<TorrentRow> {
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
}

fn escape_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn build_where(p: &ListParams) -> (String, Vec<String>) {
    let (mut clauses, mut args) = shared_clauses(p);

    if let Some(media_type) = &p.media_type {
        append_media_type_clause(media_type, &mut clauses, &mut args);
    }
    if let Some(status) = normalized_token(p.status.as_deref()) {
        match status.as_str() {
            "running" => clauses.push("t.is_open=1".into()),
            "seeding" => clauses.push("t.complete=1 AND t.is_active=1".into()),
            "downloading" => clauses.push("t.complete=0 AND t.is_active=1".into()),
            "completed" => clauses.push("t.complete=1".into()),
            "active" | "resumed" => clauses.push("t.is_active=1".into()),
            "inactive" => clauses.push("t.is_active=0".into()),
            "queued" => clauses.push("(t.state=5 OR (t.state=1 AND t.is_open=0))".into()),
            "paused" | "stopped" => clauses.push("t.state=0 AND t.is_active=0".into()),
            // Kept in sync with the identical bucket definitions in
            // sidebar_facets() above -- see the comment there.
            "stalled" => clauses.push(STALLED_SQL.into()),
            "stalled_uploading" => clauses.push(STALLED_UPLOADING_SQL.into()),
            "stalled_downloading" => clauses.push(STALLED_DOWNLOADING_SQL.into()),
            "checking" => clauses.push("t.state=2".into()),
            "moving" => clauses.push("0=1".into()),
            "error" | "errored" => clauses.push("t.state=3".into()),
            // Kept in sync with sidebar_facets()'s identical bucket -- see
            // the comment there for why this is distinct from "error".
            "tracker_error" => clauses.push(TRACKER_ERROR_SQL.into()),
            // Never turn an unsupported status into an unfiltered library
            // query. TorrentNG API status values are case-insensitive and
            // invalid values fail closed at the predicate layer.
            _ => clauses.push("0".into()),
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };

    (where_sql, args)
}

/// Known media-type bucket keys. Anything else classifies as "no match"
/// rather than silently matching everything.
const KNOWN_MEDIA_TYPES: &[&str] = &[
    "ebook", "tv", "video", "audio", "image", "game", "software", "other",
];

fn append_media_type_clause(media_type: &str, clauses: &mut Vec<String>, args: &mut Vec<String>) {
    if !KNOWN_MEDIA_TYPES.contains(&media_type) {
        clauses.push("0".to_owned());
        return;
    }
    let idx = args.len() + 1;
    clauses.push(format!(
        "tng_media_type_match(
            t.name,
            t.category,
            t.directory,
            COALESCE((
                SELECT GROUP_CONCAT(ordered_tags.tag)
                FROM (
                    SELECT tt.tag
                    FROM torrent_tags tt
                    WHERE tt.hash=t.hash
                    ORDER BY tt.tag COLLATE NOCASE, tt.tag
                ) AS ordered_tags
            ), ''),
            ?{idx}
        ) = 1"
    ));
    args.push(media_type.to_owned());
}

fn order_clause(sort: Option<&str>, dir: Option<&str>) -> String {
    let sort = normalized_token(sort);
    let col = match sort.as_deref() {
        Some("name") => "t.name COLLATE NOCASE",
        Some("size") => "t.size_bytes",
        Some("remaining") => "(t.size_bytes - t.bytes_done)",
        Some("added") => "t.creation_date",
        Some("completed") => "t.timestamp_finished",
        Some("ratio") => "t.ratio",
        Some("speed_down") => EFFECTIVE_DOWN_RATE_SQL,
        Some("speed_up") => EFFECTIVE_UP_RATE_SQL,
        Some("seeds") => "t.peers_complete",
        Some("peers") => "t.peers_connected",
        Some("progress") => "CAST(t.bytes_done AS REAL) / NULLIF(t.size_bytes, 0)",
        // Mirrors the WebUI's statusLabel() precedence (TorrentTable.tsx)
        // exactly, so sorting by status matches what the column displays.
        Some("status") => {
            "CASE \
                WHEN t.state = 3 THEN 0 \
                WHEN t.state = 0 THEN 1 \
                WHEN t.state = 2 THEN 2 \
                WHEN t.state = 4 THEN 3 \
                WHEN (t.state = 5 OR (t.state = 1 AND t.is_open = 0)) THEN 4 \
                WHEN t.complete = 1 AND t.is_active = 1 THEN 5 \
                WHEN t.complete = 0 AND t.is_active = 1 THEN 6 \
                WHEN t.is_open = 1 THEN 7 \
                ELSE 8 \
            END"
        }
        _ => "t.name COLLATE NOCASE",
    };
    let d = if dir
        .map(str::trim)
        .is_some_and(|dir| dir.eq_ignore_ascii_case("desc"))
    {
        "DESC"
    } else {
        "ASC"
    };
    // Offset pagination needs a total ordering.  Without a unique tie-breaker
    // SQLite may return equal names/statuses/rates in different orders across
    // pages, which makes rows appear twice or disappear between requests.
    format!("{col} {d}, t.hash COLLATE NOCASE {d}")
}

fn normalized_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tracker_error_integration_tests {
    use super::{bounded_page_limit, Db, ListParams, MAX_API_PAGE_ENTRIES};
    use crate::cache::db::TorrentRow;

    fn row(hash: &str, is_active: bool, message: &str) -> TorrentRow {
        TorrentRow {
            hash: hash.to_owned(),
            name: format!("torrent-{hash}"),
            size_bytes: 1_000_000,
            bytes_done: 1_000_000,
            down_rate: 0,
            up_rate: 12_345,
            up_total: 5_000_000,
            down_total: 1_000_000,
            ratio: 5000,
            is_active,
            is_open: is_active,
            complete: true,
            state: if is_active { 1 } else { 0 },
            priority: 3,
            category: String::new(),
            base_path: "/data".to_owned(),
            directory: "/data".to_owned(),
            creation_date: 0,
            timestamp_finished: 0,
            tracker_focus: 0,
            peers_connected: 0,
            peers_complete: 1,
            message: message.to_owned(),
            tracker_url: "https://tracker.example/announce".to_owned(),
            tags: String::new(),
            updated_at: 0,
        }
    }

    /// TNG-webui: a torrent can be actively seeding fine while its
    /// tracker rejects announces (e.g. "torrent not registered with this
    /// tracker"). Proves the separate "tracker_error" bucket surfaces
    /// this against a real SQLite-backed cache, not just a generated SQL
    /// string.
    #[test]
    fn seeding_torrent_with_tracker_failure_is_findable_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&row(
            "a".repeat(40).as_str(),
            true,
            "Tracker: [Failure reason \"torrent not registered with this tracker\"]",
        ))
        .unwrap();
        db.upsert(&row("b".repeat(40).as_str(), true, "")).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.status.get("tracker_error"), Some(&1));
        // A tracker warning is not a terminal torrent error.
        assert_eq!(facets.status.get("error"), Some(&0));

        let (rows, total) = db
            .list(&ListParams {
                status: Some("tracker_error".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "a".repeat(40));
    }

    #[test]
    fn terminal_error_without_message_is_findable_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut terminal = row("terminal", false, "");
        terminal.state = 3;
        terminal.complete = false;
        db.upsert(&terminal).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.status.get("error"), Some(&1));
        assert_eq!(facets.status.get("stopped"), Some(&0));

        let (rows, total) = db
            .list(&ListParams {
                status: Some("error".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "terminal");

        let (errored, stopped, seeding, downloading, _) = db.sync_counts().unwrap();
        assert_eq!((errored, stopped, seeding, downloading), (1, 0, 0, 0));
    }

    #[test]
    fn whitespace_tracker_message_is_not_reported_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&row("whitespace", true, "   ")).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.status.get("tracker_error"), Some(&0));
        let (rows, total) = db
            .list(&ListParams {
                status: Some("tracker_error".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 0);
        assert!(rows.is_empty());

        let health = db.tracker_health().unwrap();
        assert_eq!(health.len(), 1);
        assert_eq!(health[0].error_count, 0);
    }

    #[test]
    fn live_stats_reads_requested_rows_in_one_projection() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let hash = "a".repeat(40);
        let mut torrent = row(&hash, true, "");
        torrent.size_bytes = 100;
        torrent.bytes_done = 40;
        torrent.down_rate = 12_345;
        torrent.up_rate = 678;
        db.upsert(&torrent).unwrap();

        let rows = db.live_stats(&[hash.to_ascii_uppercase()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, hash);
        assert_eq!(rows[0].size_bytes - rows[0].bytes_done, 60);
        assert_eq!(rows[0].down_rate, 12_345);
        assert_eq!(rows[0].up_rate, 678);
    }

    #[test]
    fn stale_rates_match_stalled_filter_and_sort_as_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();

        let mut stale = row("stale", true, "");
        stale.up_rate = 12_345;
        stale.updated_at = 0;
        db.upsert(&stale).unwrap();

        let mut fresh = row("fresh", true, "");
        fresh.up_rate = 1;
        fresh.updated_at = chrono::Utc::now().timestamp();
        db.upsert(&fresh).unwrap();

        let (stalled, total) = db
            .list(&ListParams {
                status: Some("stalled_uploading".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(
            stalled.iter().map(|row| row.hash.as_str()).collect::<Vec<_>>(),
            vec!["stale"]
        );

        let (ordered, _) = db
            .list(&ListParams {
                sort: Some("speed_up".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            ordered.iter().map(|row| row.hash.as_str()).collect::<Vec<_>>(),
            vec!["stale", "fresh"]
        );
    }

    #[test]
    fn tracker_filter_treats_sql_wildcards_as_literal_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut literal = row("a", true, "");
        literal.tracker_url = "https://tracker.example/announce?token=100%".to_owned();
        db.upsert(&literal).unwrap();
        db.upsert(&row("b", true, "")).unwrap();

        let (rows, total) = db
            .list(&ListParams {
                tracker: Some("100%".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "a");
    }

    #[test]
    fn name_filter_treats_sql_wildcards_as_literal_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut literal = row("literal", true, "");
        literal.name = "100%_literal".to_owned();
        db.upsert(&literal).unwrap();
        let mut wildcard = row("wildcard", true, "");
        wildcard.name = "1000x_literal".to_owned();
        db.upsert(&wildcard).unwrap();

        let (rows, total) = db
            .list(&ListParams {
                filter: Some("100%_".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].hash, "literal");
    }

    #[test]
    fn filter_matches_info_hash_and_facets() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let hash = "ABCDEF1234567890";
        let mut torrent = row(hash, true, "");
        torrent.name = "Unrelated name".to_owned();
        db.upsert(&torrent).unwrap();

        let params = ListParams {
            filter: Some("abcdef1234".to_owned()),
            ..Default::default()
        };
        let (rows, total) = db.list(&params).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, hash);

        let facets = db.sidebar_facets(&params).unwrap();
        assert_eq!(facets.status.get("all"), Some(&1));
    }

    #[test]
    fn other_media_type_is_queryable_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut other = row("other", true, "");
        other.name = "Plain document".to_owned();
        db.upsert(&other).unwrap();
        let mut video = row("video", true, "");
        video.name = "Movie.mkv".to_owned();
        db.upsert(&video).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.media_type.get("other"), Some(&1));

        let (rows, total) = db
            .list(&ListParams {
                media_type: Some("other".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "other");
    }

    #[test]
    fn native_queued_state_is_queryable_alongside_legacy_queue_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let mut queued = row("torrentng-queued", false, "");
        queued.complete = false;
        queued.state = 5;
        queued.is_open = false;
        db.upsert(&queued).unwrap();

        let mut legacy = row("legacy-queued", false, "");
        legacy.complete = false;
        legacy.state = 1;
        legacy.is_open = false;
        db.upsert(&legacy).unwrap();

        let facets = db.sidebar_facets(&ListParams::default()).unwrap();
        assert_eq!(facets.status.get("queued"), Some(&2));
        let (rows, total) = db
            .list(&ListParams {
                status: Some("queued".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn bounded_incremental_listing_rejects_truncation_and_orders_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let since = db.current_revision().unwrap();
        for (hash, updated_at) in [("b", 2), ("a", 1)] {
            let mut torrent = row(hash, true, "");
            torrent.updated_at = updated_at;
            db.upsert(&torrent).unwrap();
        }

        assert!(db.list_since_bounded(since, 1).unwrap().is_none());

        let delta = db
            .list_since_bounded(since, 2)
            .unwrap()
            .expect("the complete delta fits within the bound");
        assert_eq!(
            delta
                .changed
                .iter()
                .map(|row| row.hash.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert!(delta.revision > since);
        assert!(delta.removed.is_empty());
    }

    #[test]
    fn bounded_incremental_listing_with_zero_capacity_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        db.upsert(&row("a", true, "")).unwrap();
        let since = db.current_revision().unwrap();
        assert!(db.list_since_bounded(since, 0).unwrap().is_none());
    }

    #[test]
    fn durable_revision_captures_same_second_updates_and_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("cache.db")).unwrap();
        let since = db.current_revision().unwrap();

        let mut first = row("first", true, "");
        first.updated_at = 1_700_000_000;
        db.upsert(&first).unwrap();
        let mut second = row("second", true, "");
        second.updated_at = first.updated_at;
        db.upsert(&second).unwrap();

        let delta = db
            .list_since_bounded(since, 10)
            .unwrap()
            .expect("same-second updates remain individually discoverable");
        assert_eq!(delta.changed.len(), 2);
        assert!(delta
            .changed
            .iter()
            .all(|row| row.updated_at == 1_700_000_000));

        let after_updates = delta.revision;
        db.delete("first").unwrap();
        let delta = db
            .list_since_bounded(after_updates, 10)
            .unwrap()
            .expect("deletion tombstone is replayable");
        assert_eq!(delta.removed, vec!["first"]);
        assert!(delta.changed.is_empty());
    }

    #[test]
    fn public_page_limit_is_bounded() {
        assert_eq!(bounded_page_limit(None), None);
        assert_eq!(bounded_page_limit(Some(0)), Some(1));
        assert_eq!(
            bounded_page_limit(Some(MAX_API_PAGE_ENTRIES + 1)),
            Some(MAX_API_PAGE_ENTRIES)
        );
    }
}

#[cfg(test)]
mod status_bucket_tests {
    use super::{build_where, ListParams, TRACKER_ERROR_SQL};

    fn where_for(status: &str) -> String {
        let params = ListParams {
            status: Some(status.to_owned()),
            ..Default::default()
        };
        build_where(&params).0
    }

    #[test]
    fn seeding_excludes_incomplete_and_stopped() {
        let w = where_for("seeding");
        assert!(w.contains("t.complete=1"), "{w}");
        assert!(w.contains("t.is_active=1"), "{w}");
    }

    #[test]
    fn downloading_excludes_complete() {
        let w = where_for("downloading");
        assert!(w.contains("t.complete=0"), "{w}");
        assert!(w.contains("t.is_active=1"), "{w}");
    }

    #[test]
    fn stalled_uploading_checks_throughput_not_started_state() {
        let w = where_for("stalled_uploading");
        assert!(w.contains("t.up_rate <= 0"), "{w}");
        assert!(w.contains("t.complete=1"), "{w}");
        // The bug this guards against: checking is_active=0 (stopped)
        // instead of up_rate=0 (zero throughput while still running).
        assert!(
            !w.contains("is_active=0"),
            "stalled_uploading must not require is_active=0 (that means stopped, not stalled): {w}"
        );
    }

    #[test]
    fn stalled_downloading_checks_throughput_not_started_state() {
        let w = where_for("stalled_downloading");
        assert!(w.contains("t.down_rate <= 0"), "{w}");
        assert!(w.contains("t.complete=0"), "{w}");
        assert!(
            !w.contains("is_active=0"),
            "stalled_downloading must not require is_active=0 (that means stopped, not stalled): {w}"
        );
    }

    #[test]
    fn tracker_error_matches_regardless_of_active_state() {
        // The bug this guards against: a torrent actively seeding fine
        // except for a rejected tracker announce (e.g. "torrent not
        // registered with this tracker") must still be matched -- unlike
        // "error"/"errored", this must NOT require is_active=0.
        let w = where_for("tracker_error");
        assert!(w.contains(TRACKER_ERROR_SQL), "{w}");
        assert!(
            !w.contains("is_active"),
            "tracker_error must match active torrents with a tracker message too: {w}"
        );
    }

    #[test]
    fn error_and_tracker_error_are_distinct_buckets() {
        // "error" is the terminal state bucket; tracker_error remains a
        // separate message-based tracker-health bucket.
        let error = where_for("error");
        assert!(error.contains("t.state=3"), "{error}");
        assert!(!error.contains(TRACKER_ERROR_SQL), "{error}");
        assert!(!error.contains("t.is_active=0"), "{error}");
    }

    #[test]
    fn status_tokens_are_trimmed_and_case_insensitive() {
        let where_sql = where_for("  ErRoReD  ");
        assert!(where_sql.contains("t.state=3"), "{where_sql}");
    }

    #[test]
    fn unknown_status_fails_closed_instead_of_matching_everything() {
        let where_sql = where_for("not-a-real-status");
        assert!(where_sql.contains("0"), "{where_sql}");
    }
}

#[cfg(test)]
mod order_clause_tests {
    use super::order_clause;

    #[test]
    fn status_sorts_via_case_expression_not_default_name_sort() {
        let clause = order_clause(Some("status"), None);
        assert!(
            clause.contains("CASE"),
            "expected a CASE expression: {clause}"
        );
        assert!(
            clause.contains("END ASC, t.hash COLLATE NOCASE ASC"),
            "unexpected clause: {clause}"
        );
        assert!(
            !clause.contains("t.name"),
            "status sort must not silently fall back to name sort: {clause}"
        );
    }

    #[test]
    fn status_desc_respects_direction() {
        let clause = order_clause(Some("status"), Some("desc"));
        assert!(
            clause.contains("END DESC, t.hash COLLATE NOCASE DESC"),
            "unexpected clause: {clause}"
        );
    }

    #[test]
    fn status_sort_matches_error_metadata_and_queue_labels() {
        let clause = order_clause(Some("status"), None);
        assert!(
            clause.contains("WHEN t.state = 3 THEN 0"),
            "terminal errors must sort with the displayed error bucket: {clause}"
        );
        assert!(
            !clause.contains("t.message <> ''"),
            "message-only tracker failures must remain separate from terminal errors: {clause}"
        );
        assert!(
            clause.contains("WHEN t.state = 4 THEN 3"),
            "metadata-pending rows must have their displayed status rank: {clause}"
        );
        assert!(
            clause.contains("WHEN (t.state = 5 OR (t.state = 1 AND t.is_open = 0)) THEN 4"),
            "queued rows must have their displayed status rank: {clause}"
        );
    }

    #[test]
    fn unknown_sort_falls_back_to_name() {
        let clause = order_clause(Some("not-a-real-column"), None);
        assert_eq!(
            clause,
            "t.name COLLATE NOCASE ASC, t.hash COLLATE NOCASE ASC"
        );
    }
}

#[cfg(test)]
mod page_bound_tests {
    use super::{validate_page_offset, MAX_API_PAGE_OFFSET};

    #[test]
    fn page_offset_is_finite_and_non_negative() {
        assert_eq!(validate_page_offset(None).unwrap(), 0);
        assert_eq!(
            validate_page_offset(Some(MAX_API_PAGE_OFFSET)).unwrap(),
            MAX_API_PAGE_OFFSET
        );
        assert!(validate_page_offset(Some(-1)).is_err());
        assert!(validate_page_offset(Some(MAX_API_PAGE_OFFSET + 1)).is_err());
    }
}
