use anyhow::Result;
use rusqlite::params_from_iter;
use serde::Deserialize;

use super::db::{Db, TorrentRow};

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub filter: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl Db {
    pub fn list(&self, p: &ListParams) -> Result<(Vec<TorrentRow>, i64)> {
        let (where_sql, args) = build_where(p);
        let order = order_clause(p.sort.as_deref(), p.dir.as_deref());
        let limit = p.limit.unwrap_or(100).clamp(1, 1000);
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
                    COALESCE((SELECT GROUP_CONCAT(tt.tag ORDER BY tt.tag)
                               FROM torrent_tags tt WHERE tt.hash=t.hash), '') AS tags,
                    t.updated_at
             FROM torrents t{where_sql} ORDER BY {order} LIMIT ?{n1} OFFSET ?{n2}",
            n1 = args.len() + 1,
            n2 = args.len() + 2,
        );

        let mut all_args: Vec<String> = args;
        all_args.push(limit.to_string());
        all_args.push(offset.to_string());

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(all_args.iter()), |r: &rusqlite::Row<'_>| {
            Ok(TorrentRow {
                hash:               r.get(0)?,
                name:               r.get(1)?,
                size_bytes:         r.get(2)?,
                bytes_done:         r.get(3)?,
                down_rate:          r.get(4)?,
                up_rate:            r.get(5)?,
                up_total:           r.get(6)?,
                down_total:         r.get(7)?,
                ratio:              r.get(8)?,
                is_active:          r.get::<_, i64>(9)? != 0,
                is_open:            r.get::<_, i64>(10)? != 0,
                complete:           r.get::<_, i64>(11)? != 0,
                state:              r.get(12)?,
                priority:           r.get(13)?,
                category:           r.get(14)?,
                base_path:          r.get(15)?,
                directory:          r.get(16)?,
                creation_date:      r.get(17)?,
                timestamp_finished: r.get(18)?,
                tracker_focus:      r.get(19)?,
                peers_connected:    r.get(20)?,
                peers_complete:     r.get(21)?,
                message:            r.get(22)?,
                tracker_url:        r.get(23)?,
                tags:               r.get(24)?,
                updated_at:         r.get(25)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok((rows, total))
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
    if let Some(status) = &p.status {
        match status.as_str() {
            "seeding"      => clauses.push("t.complete=1 AND t.is_active=1".into()),
            "downloading"  => clauses.push("t.complete=0 AND t.is_active=1".into()),
            "stopped"      => clauses.push("t.is_active=0".into()),
            "checking"     => clauses.push("t.state=2".into()),
            "error"        => clauses.push("t.message != '' AND t.is_active=0".into()),
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

fn order_clause(sort: Option<&str>, dir: Option<&str>) -> String {
    let col = match sort {
        Some("name")       => "t.name COLLATE NOCASE",
        Some("size")       => "t.size_bytes",
        Some("added")      => "t.creation_date",
        Some("ratio")      => "t.ratio",
        Some("speed_down") => "t.down_rate",
        Some("speed_up")   => "t.up_rate",
        Some("progress")   => "CAST(t.bytes_done AS REAL) / NULLIF(t.size_bytes, 0)",
        _                  => "t.name COLLATE NOCASE",
    };
    let d = if dir.map(|d| d.eq_ignore_ascii_case("desc")).unwrap_or(false) { "DESC" } else { "ASC" };
    format!("{col} {d}")
}
