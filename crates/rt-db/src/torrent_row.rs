/// Torrent record stored in the `torrents` table.
use rusqlite::{
    params, params_from_iter,
    types::{Type, ValueRef},
    Connection, OptionalExtension, Params, Row,
};
use serde::{
    de::{self, Deserializer, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use std::{collections::BTreeSet, fmt, io};

use crate::error::DbError;

pub const MAX_TORRENT_LABEL_ROW_BYTES: usize = 256 * 1024;
pub const MAX_TORRENT_LABEL_RESULT_ITEMS: usize = 16_384;
pub const MAX_TORRENT_LABEL_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const TORRENT_LABEL_PAGE_SIZE: usize = 256;
pub const MAX_CATEGORY_DEFINITION_ITEMS: usize = 16_384;
pub const MAX_CATEGORY_DEFINITION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TORRENT_TRACKER_ROW_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TORRENT_TRACKER_RESULT_ITEMS: usize = 16_384;
pub const MAX_TORRENT_TRACKER_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TORRENT_TRACKER_URL_BYTES: usize = 8 * 1024;
pub const MAX_TORRENT_RESULT_ITEMS: usize = 100_000;
pub const MAX_TORRENT_RESULT_BYTES: usize = 256 * 1024 * 1024;
const MAX_TORRENT_INFO_HASH_BYTES: usize = 64;
const MAX_TORRENT_NAME_BYTES: usize = 4 * 1024;
const MAX_TORRENT_SAVE_PATH_BYTES: usize = 16 * 1024;
const MAX_TORRENT_CATEGORY_BYTES: usize = 64 * 1024;
const MAX_TORRENT_STATE_BYTES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TorrentRow {
    /// Hex-encoded 40-char SHA-1 infohash.
    pub info_hash: String,
    pub name: String,
    pub total_length: i64,
    pub piece_length: i64,
    pub piece_count: i64,
    pub is_private: bool,
    pub save_path: String,
    pub category: Option<String>,
    /// JSON-serialised `Vec<String>`.
    pub tags: Vec<String>,
    pub state: String,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    pub uploaded: i64,
    pub downloaded: i64,
    /// Live bytes still missing from the payload; unlike `downloaded`, this
    /// is not cumulative transfer accounting.
    #[serde(default)]
    pub amount_left: i64,
    pub ratio: f64,
    /// JSON-serialised `Vec<String>`.
    pub trackers: Vec<String>,
}

/// Compact label projection used by global tag mutations. Keeping this
/// separate from `TorrentRow` avoids loading metainfo-sized columns when a
/// mutation only needs category and tags.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentLabelsRow {
    pub info_hash: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub added_at: i64,
}

/// Compact projection used when the engine only needs to identify a
/// metadata placeholder or reconstruct its metadata-only task. Do not add
/// labels, save paths, or other unrelated torrent columns here: this path is
/// exercised by lifecycle admission and can run for many torrents at once.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentMetadataProjection {
    pub info_hash: String,
    pub total_length: i64,
    pub piece_count: i64,
    pub is_private: bool,
    pub state: String,
    pub completed_at: Option<i64>,
    pub amount_left: i64,
    pub trackers: Vec<String>,
}

/// Compact projection used to promote a dormant torrent. The metainfo blob
/// supplies the rest of the task state after this bounded row read.
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentPromotionProjection {
    pub info_hash: String,
    pub total_length: i64,
    pub piece_count: i64,
    pub state: String,
    pub save_path: String,
}

impl TorrentRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let tags = decode_bounded_string_vec_column(
            row,
            8,
            "torrent tags",
            MAX_TORRENT_LABEL_ROW_BYTES,
            MAX_TORRENT_LABEL_RESULT_ITEMS,
            MAX_TORRENT_LABEL_RESULT_BYTES,
            MAX_TORRENT_LABEL_ROW_BYTES,
        )?;
        let trackers = decode_bounded_string_vec_column(
            row,
            16,
            "torrent trackers",
            MAX_TORRENT_TRACKER_ROW_BYTES,
            MAX_TORRENT_TRACKER_RESULT_ITEMS,
            MAX_TORRENT_TRACKER_RESULT_BYTES,
            MAX_TORRENT_TRACKER_URL_BYTES,
        )?;
        Ok(TorrentRow {
            info_hash: bounded_text_column(
                row,
                0,
                "torrent info hash",
                MAX_TORRENT_INFO_HASH_BYTES,
            )?,
            name: bounded_text_column(row, 1, "torrent name", MAX_TORRENT_NAME_BYTES)?,
            total_length: row.get(2)?,
            piece_length: row.get(3)?,
            piece_count: row.get(4)?,
            is_private: row.get::<_, i64>(5)? != 0,
            save_path: bounded_text_column(
                row,
                6,
                "torrent save path",
                MAX_TORRENT_SAVE_PATH_BYTES,
            )?,
            category: optional_bounded_text_column(
                row,
                7,
                "torrent category",
                MAX_TORRENT_CATEGORY_BYTES,
            )?,
            tags,
            state: bounded_text_column(row, 9, "torrent state", MAX_TORRENT_STATE_BYTES)?,
            added_at: row.get(10)?,
            completed_at: row.get(11)?,
            uploaded: row.get(12)?,
            downloaded: row.get(13)?,
            amount_left: row.get(14)?,
            ratio: row.get(15)?,
            trackers,
        })
    }
}

fn column_value_error(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(column_value_error(
            column,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_text_column(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = text_value(row, column, field)? else {
        return Err(column_value_error(column, format!("{field} is NULL")));
    };
    if value.len() > maximum {
        return Err(column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| column_value_error(column, format!("{field} is not UTF-8: {error}")))
}

fn optional_bounded_text_column(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<Option<String>> {
    let Some(value) = text_value(row, column, field)? else {
        return Ok(None);
    };
    if value.len() > maximum {
        return Err(column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(|value| Some(value.to_owned()))
        .map_err(|error| column_value_error(column, format!("{field} is not UTF-8: {error}")))
}

struct BoundedStringVecVisitor {
    field: &'static str,
    max_items: usize,
    max_bytes: usize,
    max_item_bytes: usize,
}

impl<'de> Visitor<'de> for BoundedStringVecVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON array of strings")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values =
            Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(self.max_items));
        let mut total_bytes = 0usize;
        while let Some(value) = sequence.next_element::<String>()? {
            if values.len() >= self.max_items {
                return Err(de::Error::custom(format!(
                    "{} contains more than {} items",
                    self.field, self.max_items
                )));
            }
            if value.len() > self.max_item_bytes {
                return Err(de::Error::custom(format!(
                    "{} item contains {} bytes; maximum is {}",
                    self.field,
                    value.len(),
                    self.max_item_bytes
                )));
            }
            total_bytes = total_bytes.saturating_add(value.len());
            if total_bytes > self.max_bytes {
                return Err(de::Error::custom(format!(
                    "{} contains {} bytes; maximum is {}",
                    self.field, total_bytes, self.max_bytes
                )));
            }
            values.push(value);
        }
        Ok(values)
    }
}

fn decode_bounded_string_vec_column(
    row: &Row<'_>,
    column: usize,
    field: &'static str,
    max_json_bytes: usize,
    max_items: usize,
    max_bytes: usize,
    max_item_bytes: usize,
) -> rusqlite::Result<Vec<String>> {
    let Some(value) = text_value(row, column, field)? else {
        return Err(column_value_error(column, format!("{field} is NULL")));
    };
    if value.len() > max_json_bytes {
        return Err(column_value_error(
            column,
            format!(
                "{field} JSON is {} bytes; maximum is {max_json_bytes}",
                value.len()
            ),
        ));
    }
    let value = std::str::from_utf8(value).map_err(|error| {
        column_value_error(column, format!("{field} JSON is not UTF-8: {error}"))
    })?;
    let mut deserializer = serde_json::Deserializer::from_str(value);
    let decoded = deserializer
        .deserialize_seq(BoundedStringVecVisitor {
            field,
            max_items,
            max_bytes,
            max_item_bytes,
        })
        .map_err(|error| column_value_error(column, format!("{field} JSON is invalid: {error}")))?;
    deserializer.end().map_err(|error| {
        column_value_error(column, format!("{field} JSON has trailing data: {error}"))
    })?;
    Ok(decoded)
}

pub fn upsert(conn: &Connection, row: &TorrentRow) -> Result<(), DbError> {
    let tags_json = serde_json::to_string(&row.tags)?;
    let trackers_json = serde_json::to_string(&row.trackers)?;
    conn.execute(
        "INSERT INTO torrents
            (info_hash, name, total_length, piece_length, piece_count, is_private,
            save_path, category, tags, state, added_at, completed_at,
             uploaded, downloaded, amount_left, ratio, trackers)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(info_hash) DO UPDATE SET
             name=excluded.name,
             total_length=excluded.total_length,
             piece_length=excluded.piece_length,
             piece_count=excluded.piece_count,
             is_private=excluded.is_private,
             save_path=excluded.save_path,
             state=excluded.state,
             uploaded=excluded.uploaded, downloaded=excluded.downloaded,
             amount_left=excluded.amount_left,
             ratio=excluded.ratio, completed_at=excluded.completed_at,
             category=excluded.category, tags=excluded.tags,
             trackers=excluded.trackers",
        params![
            row.info_hash,
            row.name,
            row.total_length,
            row.piece_length,
            row.piece_count,
            row.is_private as i64,
            row.save_path,
            row.category,
            tags_json,
            row.state,
            row.added_at,
            row.completed_at,
            row.uploaded,
            row.downloaded,
            row.amount_left,
            row.ratio,
            trackers_json,
        ],
    )?;
    persist_normalized_labels(conn, row)?;
    Ok(())
}

pub fn upsert_in_tx(tx: &rusqlite::Transaction<'_>, row: &TorrentRow) -> Result<(), DbError> {
    let tags_json = serde_json::to_string(&row.tags)?;
    let trackers_json = serde_json::to_string(&row.trackers)?;
    tx.execute(
        "INSERT INTO torrents
            (info_hash, name, total_length, piece_length, piece_count, is_private,
            save_path, category, tags, state, added_at, completed_at,
             uploaded, downloaded, amount_left, ratio, trackers)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
         ON CONFLICT(info_hash) DO UPDATE SET
             name=excluded.name,
             total_length=excluded.total_length,
             piece_length=excluded.piece_length,
             piece_count=excluded.piece_count,
             is_private=excluded.is_private,
             save_path=excluded.save_path,
             state=excluded.state,
             uploaded=excluded.uploaded, downloaded=excluded.downloaded,
             amount_left=excluded.amount_left,
             ratio=excluded.ratio, completed_at=excluded.completed_at,
             category=excluded.category, tags=excluded.tags,
             trackers=excluded.trackers",
        params![
            row.info_hash,
            row.name,
            row.total_length,
            row.piece_length,
            row.piece_count,
            row.is_private as i64,
            row.save_path,
            row.category,
            tags_json,
            row.state,
            row.added_at,
            row.completed_at,
            row.uploaded,
            row.downloaded,
            row.amount_left,
            row.ratio,
            trackers_json,
        ],
    )?;
    persist_normalized_labels_in_tx(tx, row)?;
    Ok(())
}

/// Update only the fields owned by a running torrent task.
///
/// Runtime progress is written from a snapshot that may have been taken
/// before the engine actor persisted a concurrent label/path/tracker change.
/// Keeping this update partial prevents that stale snapshot from overwriting
/// actor-owned metadata. It also deliberately refuses to insert a missing
/// row: a task finishing after removal must not resurrect the torrent.
pub fn update_runtime_in_tx(
    tx: &rusqlite::Transaction<'_>,
    row: &TorrentRow,
) -> Result<bool, DbError> {
    let changed = tx.execute(
        "UPDATE torrents
         SET state = ?2,
             completed_at = ?3,
             uploaded = ?4,
             downloaded = ?5,
             amount_left = ?6,
             ratio = ?7,
             total_length = ?8
         WHERE info_hash = ?1",
        params![
            row.info_hash,
            row.state,
            row.completed_at,
            row.uploaded,
            row.downloaded,
            row.amount_left,
            row.ratio,
            row.total_length,
        ],
    )?;
    Ok(changed > 0)
}

/// Update only lifecycle-owned fields on an existing torrent row.
pub fn update_state_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    state: &str,
    completed_at: Option<i64>,
    total_length: i64,
    amount_left: i64,
) -> Result<bool, DbError> {
    let changed = tx.execute(
        "UPDATE torrents
         SET state = ?2,
             completed_at = ?3,
             total_length = ?4,
             amount_left = ?5
         WHERE info_hash = ?1",
        params![info_hash, state, completed_at, total_length, amount_left,],
    )?;
    Ok(changed > 0)
}

/// Update only the lifecycle state on an existing torrent row. This is used
/// when a runtime task fails and the engine must not load or rewrite any
/// metainfo-sized columns just to isolate that task.
pub fn update_state_only_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    state: &str,
) -> Result<bool, DbError> {
    let changed = tx.execute(
        "UPDATE torrents SET state = ?2 WHERE info_hash = ?1",
        params![info_hash, state],
    )?;
    Ok(changed > 0)
}

/// Update only user-editable name and save-path fields on an existing row.
pub fn update_fields_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    name: &str,
    save_path: &str,
) -> Result<bool, DbError> {
    let changed = tx.execute(
        "UPDATE torrents
         SET name = ?2,
             save_path = ?3
         WHERE info_hash = ?1",
        params![info_hash, name, save_path],
    )?;
    Ok(changed > 0)
}

/// Update labels on an existing row and keep the normalized tag projection in sync.
pub fn update_labels_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    category: &Option<String>,
    tags: &[String],
    added_at: i64,
) -> Result<bool, DbError> {
    let tags_json = serde_json::to_string(tags)?;
    let changed = tx.execute(
        "UPDATE torrents
         SET category = ?2,
             tags = ?3
         WHERE info_hash = ?1",
        params![info_hash, category, tags_json],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    persist_normalized_labels_values_in_tx(tx, info_hash, category, tags, added_at)?;
    Ok(true)
}

/// Update the compact tracker JSON on an existing row.
pub fn update_trackers_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    trackers: &[String],
) -> Result<bool, DbError> {
    let trackers_json = serde_json::to_string(trackers)?;
    let changed = tx.execute(
        "UPDATE torrents SET trackers = ?2 WHERE info_hash = ?1",
        params![info_hash, trackers_json],
    )?;
    Ok(changed > 0)
}

fn persist_normalized_labels(conn: &Connection, row: &TorrentRow) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM torrent_tags WHERE info_hash = ?1",
        params![row.info_hash],
    )?;
    for tag in &row.tags {
        conn.execute(
            "INSERT OR IGNORE INTO torrent_tags (info_hash, tag) VALUES (?1, ?2)",
            params![row.info_hash, tag],
        )?;
    }
    if let Some(category) = &row.category {
        conn.execute(
            "INSERT OR IGNORE INTO torrent_categories (name, save_path, created_at)
             VALUES (?1, NULL, ?2)",
            params![category, row.added_at],
        )?;
    }
    Ok(())
}

fn persist_normalized_labels_in_tx(
    tx: &rusqlite::Transaction<'_>,
    row: &TorrentRow,
) -> Result<(), DbError> {
    persist_normalized_labels_values_in_tx(
        tx,
        &row.info_hash,
        &row.category,
        &row.tags,
        row.added_at,
    )
}

fn persist_normalized_labels_values_in_tx(
    tx: &rusqlite::Transaction<'_>,
    info_hash: &str,
    category: &Option<String>,
    tags: &[String],
    added_at: i64,
) -> Result<(), DbError> {
    tx.execute(
        "DELETE FROM torrent_tags WHERE info_hash = ?1",
        params![info_hash],
    )?;
    for tag in tags {
        tx.execute(
            "INSERT OR IGNORE INTO torrent_tags (info_hash, tag) VALUES (?1, ?2)",
            params![info_hash, tag],
        )?;
    }
    if let Some(category) = category {
        tx.execute(
            "INSERT OR IGNORE INTO torrent_categories (name, save_path, created_at)
             VALUES (?1, NULL, ?2)",
            params![category, added_at],
        )?;
    }
    Ok(())
}

pub fn list_torrent_tags(conn: &Connection, info_hash: &str) -> Result<Vec<String>, DbError> {
    let mut stmt =
        conn.prepare("SELECT tag FROM torrent_tags WHERE info_hash = ?1 ORDER BY tag ASC")?;
    let tags = stmt
        .query_map(params![info_hash], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(tags)
}

/// Return all persisted torrent tags without decoding unrelated torrent
/// columns. The JSON column remains the source of truth for compatibility
/// with databases created before the normalized tag projection was populated.
pub fn list_all_torrent_tags(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut tags = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut after_rowid = 0i64;
    loop {
        let page = list_torrent_labels_page(conn, after_rowid, TORRENT_LABEL_PAGE_SIZE)?;
        let Some((last_rowid, _)) = page.last() else {
            break;
        };
        after_rowid = *last_rowid;
        for (_, row) in page {
            for tag in row.tags {
                if tag.is_empty() || tags.contains(&tag) {
                    continue;
                }
                if tags.len() >= MAX_TORRENT_LABEL_RESULT_ITEMS {
                    return Err(DbError::ValueTooLarge {
                        field: "torrent tag result",
                        len: (tags.len() + 1) as u64,
                        max: MAX_TORRENT_LABEL_RESULT_ITEMS as u64,
                    });
                }
                total_bytes = total_bytes.saturating_add(tag.len());
                if total_bytes > MAX_TORRENT_LABEL_RESULT_BYTES {
                    return Err(DbError::ValueTooLarge {
                        field: "torrent tag result",
                        len: total_bytes as u64,
                        max: MAX_TORRENT_LABEL_RESULT_BYTES as u64,
                    });
                }
                tags.insert(tag);
            }
        }
    }
    Ok(tags.into_iter().collect())
}

/// Read a bounded page of the compact label projection. The rowid cursor
/// avoids retaining every torrent label while still allowing callers to walk
/// the full table in one transaction.
pub fn list_torrent_labels_page(
    conn: &Connection,
    after_rowid: i64,
    limit: usize,
) -> Result<Vec<(i64, TorrentLabelsRow)>, DbError> {
    let limit = limit.min(TORRENT_LABEL_PAGE_SIZE);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT rowid,
                length(CAST(tags AS BLOB)),
                info_hash,
                category,
                tags,
                added_at
         FROM torrents
         WHERE rowid > ?1
         ORDER BY rowid ASC
         LIMIT ?2",
    )?;
    let mut query = stmt.query(params![after_rowid, limit as i64])?;
    let mut rows = Vec::with_capacity(limit);
    while let Some(row) = query.next()? {
        let rowid: i64 = row.get(0)?;
        let tags_bytes = row.get::<_, i64>(1)?.max(0) as u64;
        if tags_bytes > MAX_TORRENT_LABEL_ROW_BYTES as u64 {
            return Err(DbError::ValueTooLarge {
                field: "torrent label row",
                len: tags_bytes,
                max: MAX_TORRENT_LABEL_ROW_BYTES as u64,
            });
        }
        let info_hash = bounded_text_column(
            row,
            2,
            "torrent label info hash",
            MAX_TORRENT_INFO_HASH_BYTES,
        )?;
        let category = optional_bounded_text_column(
            row,
            3,
            "torrent label category",
            MAX_TORRENT_CATEGORY_BYTES,
        )?;
        let tags = decode_bounded_string_vec_column(
            row,
            4,
            "torrent labels",
            MAX_TORRENT_LABEL_ROW_BYTES,
            MAX_TORRENT_LABEL_RESULT_ITEMS,
            MAX_TORRENT_LABEL_RESULT_BYTES,
            MAX_TORRENT_LABEL_ROW_BYTES,
        )?;
        rows.push((
            rowid,
            TorrentLabelsRow {
                info_hash,
                category,
                tags,
                added_at: row.get(5)?,
            },
        ));
    }
    Ok(rows)
}

/// Return only the durable label columns for every torrent. The JSON tags
/// column remains the source of truth for compatibility with legacy rows that
/// predate the normalized tag projection.
pub fn list_all_torrent_labels(conn: &Connection) -> Result<Vec<TorrentLabelsRow>, DbError> {
    let mut all = Vec::new();
    let mut after_rowid = 0i64;
    loop {
        let page = list_torrent_labels_page(conn, after_rowid, TORRENT_LABEL_PAGE_SIZE)?;
        let Some((last_rowid, _)) = page.last() else {
            break;
        };
        after_rowid = *last_rowid;
        all.extend(page.into_iter().map(|(_, row)| row));
    }
    Ok(all)
}

pub fn list_categories(conn: &Connection) -> Result<Vec<String>, DbError> {
    Ok(list_category_definitions(conn)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// Return the durable category definitions, including their optional default
/// save paths.  Torrent labels are deliberately kept separate from these
/// definitions: a category can exist before any torrent uses it.
pub fn list_category_definitions(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>)>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT name,
                length(CAST(name AS BLOB)),
                save_path,
                COALESCE(length(CAST(save_path AS BLOB)), 0)
         FROM torrent_categories
         ORDER BY name ASC
         LIMIT ?1",
    )?;
    let mut query = stmt.query(params![MAX_CATEGORY_DEFINITION_ITEMS as i64 + 1])?;
    let mut categories = Vec::new();
    let mut total_bytes = 0usize;
    while let Some(row) = query.next()? {
        if categories.len() >= MAX_CATEGORY_DEFINITION_ITEMS {
            return Err(DbError::ValueTooLarge {
                field: "category definition result",
                len: (categories.len() + 1) as u64,
                max: MAX_CATEGORY_DEFINITION_ITEMS as u64,
            });
        }
        let name_bytes = row.get::<_, i64>(1)?.max(0) as u64;
        let save_path_bytes = row.get::<_, i64>(3)?.max(0) as u64;
        let row_bytes = name_bytes.saturating_add(save_path_bytes);
        if row_bytes > MAX_TORRENT_LABEL_ROW_BYTES as u64 {
            return Err(DbError::ValueTooLarge {
                field: "category definition row",
                len: row_bytes,
                max: MAX_TORRENT_LABEL_ROW_BYTES as u64,
            });
        }
        total_bytes = total_bytes.saturating_add(row_bytes as usize);
        if total_bytes > MAX_CATEGORY_DEFINITION_BYTES {
            return Err(DbError::ValueTooLarge {
                field: "category definition result",
                len: total_bytes as u64,
                max: MAX_CATEGORY_DEFINITION_BYTES as u64,
            });
        }
        categories.push((row.get(0)?, row.get(2)?));
    }
    Ok(categories)
}

pub fn create_category_in_tx(
    tx: &rusqlite::Transaction<'_>,
    name: &str,
    save_path: Option<&str>,
    created_at: i64,
) -> Result<(), DbError> {
    tx.execute(
        "INSERT INTO torrent_categories (name, save_path, created_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO UPDATE SET save_path=excluded.save_path",
        params![name, save_path, created_at],
    )?;
    Ok(())
}

/// Rename a category definition and all durable torrent labels in one
/// transaction.  Keeping these updates together prevents a restart from
/// exposing a category definition that disagrees with the torrent rows.
pub fn rename_category_in_tx(
    tx: &rusqlite::Transaction<'_>,
    old_name: &str,
    new_name: &str,
    save_path: Option<&str>,
) -> Result<(), DbError> {
    if old_name != new_name {
        tx.execute(
            "UPDATE torrents SET category = ?2 WHERE category = ?1",
            params![old_name, new_name],
        )?;
        tx.execute(
            "UPDATE torrent_categories
             SET name = ?2, save_path = ?3
             WHERE name = ?1",
            params![old_name, new_name, save_path],
        )?;
    } else {
        tx.execute(
            "UPDATE torrent_categories SET save_path = ?2 WHERE name = ?1",
            params![old_name, save_path],
        )?;
    }
    Ok(())
}

pub fn remove_categories_in_tx(
    tx: &rusqlite::Transaction<'_>,
    names: &[String],
) -> Result<(), DbError> {
    for name in names {
        tx.execute(
            "UPDATE torrents SET category = NULL WHERE category = ?1",
            params![name],
        )?;
        tx.execute(
            "DELETE FROM torrent_categories WHERE name = ?1",
            params![name],
        )?;
    }
    Ok(())
}

pub fn get(conn: &Connection, info_hash: &str) -> Result<TorrentRow, DbError> {
    conn.query_row(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, amount_left, ratio, trackers
         FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        TorrentRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

/// Check torrent existence without materializing the metainfo-sized row.
pub fn torrent_exists(conn: &Connection, info_hash: &str) -> Result<bool, DbError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM torrents WHERE info_hash = ?1)",
        params![info_hash],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

/// Read the privacy bit without materializing labels, tracker JSON, or other
/// user-sized torrent columns.
pub fn torrent_is_private(conn: &Connection, info_hash: &str) -> Result<bool, DbError> {
    conn.query_row(
        "SELECT is_private FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| Ok(row.get::<_, i64>(0)? != 0),
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

/// Read only the lifecycle state needed by recovery paths without loading
/// labels, tracker JSON, or other user-sized torrent columns.
pub fn torrent_state(conn: &Connection, info_hash: &str) -> Result<Option<String>, DbError> {
    conn.query_row(
        "SELECT state FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| bounded_text_column(row, 0, "torrent state", MAX_TORRENT_STATE_BYTES),
    )
    .optional()
    .map_err(DbError::from)
}

/// Read the persisted piece count needed to initialize a recheck job without
/// materializing the torrent's labels, tracker JSON, or other large fields.
pub fn torrent_piece_count(conn: &Connection, info_hash: &str) -> Result<i64, DbError> {
    conn.query_row(
        "SELECT piece_count FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| row.get(0),
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

/// Read the transfer counters needed to seed normalized tracker rows without
/// loading the torrent's labels, tracker JSON, or metainfo projection.
pub fn torrent_transfer_counters(
    conn: &Connection,
    info_hash: &str,
) -> Result<(i64, i64, i64), DbError> {
    conn.query_row(
        "SELECT uploaded, downloaded, amount_left FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

/// Read only the durable tracker override used when restoring a torrent task.
/// The full torrent row includes labels and other user-sized fields that are
/// unrelated to tracker-session reconstruction.
pub fn torrent_tracker_override(
    conn: &Connection,
    info_hash: &str,
) -> Result<Option<Vec<String>>, DbError> {
    conn.query_row(
        "SELECT trackers FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| {
            decode_bounded_string_vec_column(
                row,
                0,
                "torrent trackers",
                MAX_TORRENT_TRACKER_ROW_BYTES,
                MAX_TORRENT_TRACKER_RESULT_ITEMS,
                MAX_TORRENT_TRACKER_RESULT_BYTES,
                MAX_TORRENT_TRACKER_URL_BYTES,
            )
        },
    )
    .optional()
    .map_err(DbError::from)
}

/// Read only the fields needed to classify and run a metadata placeholder.
/// This deliberately avoids labels, save paths, and the full runtime row.
pub fn torrent_metadata_projection(
    conn: &Connection,
    info_hash: &str,
) -> Result<Option<TorrentMetadataProjection>, DbError> {
    conn.query_row(
        "SELECT info_hash, total_length, piece_count, is_private, state,
                completed_at, amount_left, trackers
         FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| {
            Ok(TorrentMetadataProjection {
                info_hash: bounded_text_column(
                    row,
                    0,
                    "torrent info hash",
                    MAX_TORRENT_INFO_HASH_BYTES,
                )?,
                total_length: row.get(1)?,
                piece_count: row.get(2)?,
                is_private: row.get::<_, i64>(3)? != 0,
                state: bounded_text_column(row, 4, "torrent state", MAX_TORRENT_STATE_BYTES)?,
                completed_at: row.get(5)?,
                amount_left: row.get(6)?,
                trackers: decode_bounded_string_vec_column(
                    row,
                    7,
                    "torrent trackers",
                    MAX_TORRENT_TRACKER_ROW_BYTES,
                    MAX_TORRENT_TRACKER_RESULT_ITEMS,
                    MAX_TORRENT_TRACKER_RESULT_BYTES,
                    MAX_TORRENT_TRACKER_URL_BYTES,
                )?,
            })
        },
    )
    .optional()
    .map_err(DbError::from)
}

/// Read only the persisted fields needed before promoting a dormant torrent.
/// The potentially large metainfo blob is loaded separately after the path
/// has been authorized.
pub fn torrent_promotion_projection(
    conn: &Connection,
    info_hash: &str,
) -> Result<TorrentPromotionProjection, DbError> {
    conn.query_row(
        "SELECT info_hash, total_length, piece_count, state, save_path
         FROM torrents WHERE info_hash = ?1",
        params![info_hash],
        |row| {
            Ok(TorrentPromotionProjection {
                info_hash: bounded_text_column(
                    row,
                    0,
                    "torrent info hash",
                    MAX_TORRENT_INFO_HASH_BYTES,
                )?,
                total_length: row.get(1)?,
                piece_count: row.get(2)?,
                state: bounded_text_column(row, 3, "torrent state", MAX_TORRENT_STATE_BYTES)?,
                save_path: bounded_text_column(
                    row,
                    4,
                    "torrent save path",
                    MAX_TORRENT_SAVE_PATH_BYTES,
                )?,
            })
        },
    )
    .map_err(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(info_hash.to_owned()),
        other => DbError::Sqlite(other),
    })
}

pub fn delete(conn: &Connection, info_hash: &str) -> Result<bool, DbError> {
    let n = conn.execute(
        "DELETE FROM torrents WHERE info_hash = ?1",
        params![info_hash],
    )?;
    Ok(n > 0)
}

pub fn delete_in_tx(tx: &rusqlite::Transaction<'_>, info_hash: &str) -> Result<bool, DbError> {
    let n = tx.execute(
        "DELETE FROM torrents WHERE info_hash = ?1",
        params![info_hash],
    )?;
    Ok(n > 0)
}

pub fn list_all(conn: &Connection) -> Result<Vec<TorrentRow>, DbError> {
    list_bounded_rows(
        conn,
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, amount_left, ratio, trackers,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(info_hash AS BLOB)), 0)
                    + COALESCE(length(CAST(name AS BLOB)), 0)
                    + COALESCE(length(CAST(save_path AS BLOB)), 0)
                    + COALESCE(length(CAST(category AS BLOB)), 0)
                    + COALESCE(length(CAST(tags AS BLOB)), 0)
                    + COALESCE(length(CAST(state AS BLOB)), 0)
                    + COALESCE(length(CAST(trackers AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM torrents ORDER BY added_at DESC",
        [],
    )
}

fn list_bounded_rows<P: Params>(
    conn: &Connection,
    query: &str,
    params: P,
) -> Result<Vec<TorrentRow>, DbError> {
    let mut stmt = conn.prepare(query)?;
    let mut rows = stmt.query(params)?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(17)?.max(0) as u64;
    let result_bytes = first.get::<_, i64>(18)?.max(0) as u64;
    if result_count > MAX_TORRENT_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent result items",
            len: result_count,
            max: MAX_TORRENT_RESULT_ITEMS as u64,
        });
    }
    if result_bytes > MAX_TORRENT_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "torrent result bytes",
            len: result_bytes,
            max: MAX_TORRENT_RESULT_BYTES as u64,
        });
    }
    let mut result = Vec::with_capacity(result_count as usize);
    result.push(TorrentRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(TorrentRow::from_row(row)?);
    }
    Ok(result)
}

/// Return the small identity/privacy projection used by engine-wide DHT
/// registration sweeps. Do not load names, trackers, tags, or other
/// user-sized columns when the caller only needs to decide whether a torrent
/// may be announced through DHT.
pub fn list_torrent_privacy_for_hashes(
    conn: &Connection,
    info_hashes: &[String],
) -> Result<Vec<(String, bool)>, DbError> {
    const BATCH_SIZE: usize = 512;
    let mut projection = Vec::new();
    for batch in info_hashes.chunks(BATCH_SIZE) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT info_hash, is_private FROM torrents WHERE info_hash IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&query)?;
        let rows = stmt
            .query_map(params_from_iter(batch.iter().map(String::as_str)), |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        projection.extend(rows);
    }
    Ok(projection)
}

pub fn list_by_state(conn: &Connection, state: &str) -> Result<Vec<TorrentRow>, DbError> {
    list_bounded_rows(
        conn,
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, amount_left, ratio, trackers,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(info_hash AS BLOB)), 0)
                    + COALESCE(length(CAST(name AS BLOB)), 0)
                    + COALESCE(length(CAST(save_path AS BLOB)), 0)
                    + COALESCE(length(CAST(category AS BLOB)), 0)
                    + COALESCE(length(CAST(tags AS BLOB)), 0)
                    + COALESCE(length(CAST(state AS BLOB)), 0)
                    + COALESCE(length(CAST(trackers AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM torrents WHERE state = ?1 ORDER BY added_at DESC",
        params![state],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn sample() -> TorrentRow {
        TorrentRow {
            info_hash: "a".repeat(40),
            name: "test.torrent".into(),
            total_length: 1_000_000,
            piece_length: 262144,
            piece_count: 4,
            is_private: true,
            save_path: "/data".into(),
            category: Some("movies".into()),
            tags: vec!["hd".into(), "bluray".into()],
            state: "seeding".into(),
            added_at: 1_700_000_000,
            completed_at: Some(1_700_001_000),
            uploaded: 5_000_000,
            downloaded: 1_000_000,
            amount_left: 0,
            ratio: 5.0,
            trackers: vec!["http://tracker.example.com/announce".into()],
        }
    }

    #[test]
    fn upsert_and_get() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.name, row.name);
        assert_eq!(fetched.tags, row.tags);
        assert_eq!(fetched.trackers, row.trackers);
        assert_eq!(fetched.amount_left, row.amount_left);
        assert!(fetched.is_private);
    }

    #[test]
    fn row_reads_reject_oversized_label_json_before_deserializing_it() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        conn.execute(
            "UPDATE torrents SET tags = ?1",
            params![format!("[\"{}\"]", "x".repeat(MAX_TORRENT_LABEL_ROW_BYTES))],
        )
        .unwrap();

        assert!(get(&conn, &row.info_hash).is_err());
        assert!(list_all(&conn).is_err());
    }

    #[test]
    fn row_reads_reject_oversized_tracker_items() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        conn.execute(
            "UPDATE torrents SET trackers = ?1",
            params![format!(
                "[\"{}\"]",
                "x".repeat(MAX_TORRENT_TRACKER_URL_BYTES + 1)
            )],
        )
        .unwrap();

        assert!(get(&conn, &row.info_hash).is_err());
        assert!(list_by_state(&conn, "seeding").is_err());
    }

    #[test]
    fn cumulative_downloads_do_not_replace_live_amount_left() {
        let conn = setup();
        let mut row = sample();
        row.state = "downloading".into();
        row.total_length = 100;
        row.downloaded = 250;
        row.amount_left = 40;
        upsert(&conn, &row).unwrap();

        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.downloaded, 250);
        assert_eq!(fetched.amount_left, 40);
    }

    #[test]
    fn runtime_update_preserves_metadata_and_does_not_recreate_rows() {
        let mut conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();

        let mut runtime = row.clone();
        runtime.state = "paused".into();
        runtime.completed_at = None;
        runtime.uploaded = 7_000_000;
        runtime.downloaded = 2_000_000;
        runtime.amount_left = 123;
        runtime.ratio = 3.5;
        runtime.total_length = 2_000_000;
        let tx = conn.transaction().unwrap();
        assert!(update_runtime_in_tx(&tx, &runtime).unwrap());
        tx.commit().unwrap();

        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.name, row.name);
        assert_eq!(fetched.save_path, row.save_path);
        assert_eq!(fetched.category, row.category);
        assert_eq!(fetched.tags, row.tags);
        assert_eq!(fetched.trackers, row.trackers);
        assert_eq!(fetched.state, runtime.state);
        assert_eq!(fetched.completed_at, runtime.completed_at);
        assert_eq!(fetched.uploaded, runtime.uploaded);
        assert_eq!(fetched.downloaded, runtime.downloaded);
        assert_eq!(fetched.amount_left, runtime.amount_left);
        assert_eq!(fetched.ratio, runtime.ratio);
        assert_eq!(fetched.total_length, runtime.total_length);

        let mut missing = runtime;
        missing.info_hash = "b".repeat(40);
        let tx = conn.transaction().unwrap();
        assert!(!update_runtime_in_tx(&tx, &missing).unwrap());
        tx.commit().unwrap();
        assert!(matches!(
            get(&conn, &missing.info_hash),
            Err(DbError::NotFound(_))
        ));
    }

    #[test]
    fn upsert_persists_normalized_labels() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();

        assert_eq!(
            list_torrent_tags(&conn, &row.info_hash).unwrap(),
            vec!["bluray".to_owned(), "hd".to_owned()]
        );
        assert_eq!(list_categories(&conn).unwrap(), vec!["movies".to_owned()]);
        assert_eq!(
            list_all_torrent_tags(&conn).unwrap(),
            vec!["bluray".to_owned(), "hd".to_owned()]
        );
        assert_eq!(
            list_all_torrent_labels(&conn).unwrap(),
            vec![TorrentLabelsRow {
                info_hash: row.info_hash,
                category: row.category,
                tags: row.tags,
                added_at: row.added_at,
            }]
        );
    }

    #[test]
    fn label_update_preserves_unrelated_torrent_columns() {
        let mut conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        let category = Some("shows".to_owned());
        let tags = vec!["archive".to_owned()];

        let tx = conn.transaction().unwrap();
        assert!(update_labels_in_tx(&tx, &row.info_hash, &category, &tags, row.added_at).unwrap());
        tx.commit().unwrap();

        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.name, row.name);
        assert_eq!(fetched.save_path, row.save_path);
        assert_eq!(fetched.trackers, row.trackers);
        assert_eq!(fetched.state, row.state);
        assert_eq!(fetched.downloaded, row.downloaded);
        assert_eq!(fetched.category, category);
        assert_eq!(fetched.tags, tags);
        assert_eq!(list_torrent_tags(&conn, &row.info_hash).unwrap(), tags);
    }

    #[test]
    fn category_definitions_are_durable_and_rename_cascades_labels() {
        let mut conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        let tx = conn.transaction().unwrap();
        create_category_in_tx(&tx, "movies", Some("/srv/movies"), row.added_at).unwrap();
        tx.commit().unwrap();

        assert_eq!(
            list_category_definitions(&conn).unwrap(),
            vec![("movies".to_owned(), Some("/srv/movies".to_owned()))]
        );

        let tx = conn.transaction().unwrap();
        rename_category_in_tx(&tx, "movies", "films", Some("/srv/films")).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            get(&conn, &row.info_hash).unwrap().category.as_deref(),
            Some("films")
        );
        assert_eq!(
            list_category_definitions(&conn).unwrap(),
            vec![("films".to_owned(), Some("/srv/films".to_owned()))]
        );

        let tx = conn.transaction().unwrap();
        remove_categories_in_tx(&tx, &["films".to_owned()]).unwrap();
        tx.commit().unwrap();
        assert_eq!(get(&conn, &row.info_hash).unwrap().category, None);
        assert!(list_category_definitions(&conn).unwrap().is_empty());
    }

    #[test]
    fn upsert_replaces_normalized_tags() {
        let conn = setup();
        let mut row = sample();
        upsert(&conn, &row).unwrap();
        row.tags = vec!["archive".into()];
        upsert(&conn, &row).unwrap();

        assert_eq!(
            list_torrent_tags(&conn, &row.info_hash).unwrap(),
            vec!["archive".to_owned()]
        );
    }

    #[test]
    fn upsert_updates_state() {
        let conn = setup();
        let mut row = sample();
        upsert(&conn, &row).unwrap();
        row.state = "paused".into();
        upsert(&conn, &row).unwrap();
        let fetched = get(&conn, &row.info_hash).unwrap();
        assert_eq!(fetched.state, "paused");
    }

    #[test]
    fn get_not_found_errors() {
        let conn = setup();
        let err = get(&conn, "nonexistent").unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }

    #[test]
    fn torrent_exists_does_not_require_loading_the_full_row() {
        let conn = setup();
        let row = sample();
        assert!(!torrent_exists(&conn, &row.info_hash).unwrap());
        upsert(&conn, &row).unwrap();
        assert!(torrent_exists(&conn, &row.info_hash).unwrap());
    }

    #[test]
    fn narrow_torrent_projections_skip_large_columns() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();

        conn.execute(
            "UPDATE torrents SET name = ?1, tags = ?2",
            params![
                "n".repeat(MAX_TORRENT_NAME_BYTES + 1),
                format!("[\"{}\"]", "x".repeat(MAX_TORRENT_LABEL_ROW_BYTES + 1)),
            ],
        )
        .unwrap();

        assert!(torrent_is_private(&conn, &row.info_hash).unwrap());
        assert_eq!(
            torrent_state(&conn, &row.info_hash).unwrap(),
            Some(row.state.clone())
        );
        assert_eq!(
            torrent_piece_count(&conn, &row.info_hash).unwrap(),
            row.piece_count
        );
        assert_eq!(
            torrent_transfer_counters(&conn, &row.info_hash).unwrap(),
            (row.uploaded, row.downloaded, row.amount_left)
        );
        assert_eq!(
            torrent_tracker_override(&conn, &row.info_hash).unwrap(),
            Some(row.trackers.clone())
        );
        assert_eq!(
            torrent_metadata_projection(&conn, &row.info_hash)
                .unwrap()
                .unwrap(),
            TorrentMetadataProjection {
                info_hash: row.info_hash.clone(),
                total_length: row.total_length,
                piece_count: row.piece_count,
                is_private: row.is_private,
                state: row.state.clone(),
                completed_at: row.completed_at,
                amount_left: row.amount_left,
                trackers: row.trackers.clone(),
            }
        );
        assert_eq!(
            torrent_promotion_projection(&conn, &row.info_hash)
                .unwrap()
                .save_path,
            row.save_path
        );
    }

    #[test]
    fn delete_removes_record() {
        let conn = setup();
        upsert(&conn, &sample()).unwrap();
        let removed = delete(&conn, &sample().info_hash).unwrap();
        assert!(removed);
        assert!(get(&conn, &sample().info_hash).is_err());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let conn = setup();
        assert!(!delete(&conn, "nope").unwrap());
    }

    #[test]
    fn list_all_returns_all() {
        let conn = setup();
        let mut r1 = sample();
        let mut r2 = sample();
        r2.info_hash = "b".repeat(40);
        r2.name = "other.torrent".into();
        upsert(&conn, &r1).unwrap();
        upsert(&conn, &r2).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 2);
        // Update r1 state
        r1.state = "paused".into();
        upsert(&conn, &r1).unwrap();
        let paused = list_by_state(&conn, "paused").unwrap();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].info_hash, r1.info_hash);
    }

    #[test]
    fn torrent_lists_reject_oversized_results_before_materializing_them() {
        let mut conn = setup();
        let tx = conn.transaction().unwrap();
        for index in 0..=MAX_TORRENT_RESULT_ITEMS {
            tx.execute(
                "INSERT INTO torrents
                    (info_hash, name, total_length, piece_length, piece_count,
                     is_private, save_path, tags, state, added_at, trackers)
                 VALUES (?1, 'x', 0, 1, 0, 0, '/data', '[]', 'stopped', ?2, '[]')",
                params![format!("{index:040x}"), index as i64],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        assert!(matches!(
            list_all(&conn),
            Err(DbError::ValueTooLarge {
                field: "torrent result items",
                ..
            })
        ));
        assert!(matches!(
            list_by_state(&conn, "stopped"),
            Err(DbError::ValueTooLarge {
                field: "torrent result items",
                ..
            })
        ));
    }

    #[test]
    fn global_tag_scan_rejects_an_oversized_result() {
        let conn = setup();
        for index in 0..=MAX_TORRENT_LABEL_RESULT_BYTES / 16_384 {
            let mut row = sample();
            row.info_hash = format!("{index:040x}");
            row.tags = vec![format!("{}-{index}", "x".repeat(16_384 - 12))];
            upsert(&conn, &row).unwrap();
        }

        assert!(matches!(
            list_all_torrent_tags(&conn),
            Err(DbError::ValueTooLarge {
                field: "torrent tag result",
                ..
            })
        ));
    }

    #[test]
    fn label_page_rejects_an_oversized_corrupt_json_column() {
        let conn = setup();
        conn.execute(
            "INSERT INTO torrents
             (info_hash, name, total_length, piece_length, piece_count, is_private,
              save_path, category, tags, state, added_at, completed_at,
              uploaded, downloaded, amount_left, ratio, trackers)
             VALUES (?1, ?2, 1, 1, 1, 0, ?3, NULL, ?4, 'paused', 1, NULL, 0, 0, 1, 0, '[]')",
            params![
                "b".repeat(40),
                "bad",
                "/data",
                format!("[\"{}\"]", "x".repeat(MAX_TORRENT_LABEL_ROW_BYTES)),
            ],
        )
        .unwrap();

        assert!(matches!(
            list_torrent_labels_page(&conn, 0, 1),
            Err(DbError::ValueTooLarge {
                field: "torrent label row",
                ..
            })
        ));
    }

    #[test]
    fn label_page_rejects_oversized_identity_and_category_columns() {
        let conn = setup();
        conn.execute(
            "INSERT INTO torrents
             (info_hash, name, total_length, piece_length, piece_count, is_private,
              save_path, category, tags, state, added_at, completed_at,
              uploaded, downloaded, amount_left, ratio, trackers)
             VALUES (?1, ?2, 1, 1, 1, 0, ?3, ?4, '[]', 'paused', 1, NULL, 0, 0, 1, 0, '[]')",
            params![
                "i".repeat(MAX_TORRENT_INFO_HASH_BYTES + 1),
                "bad",
                "/data",
                "category",
            ],
        )
        .unwrap();
        assert!(list_torrent_labels_page(&conn, 0, 1).is_err());

        conn.execute("DELETE FROM torrents", []).unwrap();
        conn.execute(
            "INSERT INTO torrents
             (info_hash, name, total_length, piece_length, piece_count, is_private,
              save_path, category, tags, state, added_at, completed_at,
              uploaded, downloaded, amount_left, ratio, trackers)
             VALUES (?1, ?2, 1, 1, 1, 0, ?3, ?4, '[]', 'paused', 1, NULL, 0, 0, 1, 0, '[]')",
            params![
                "b".repeat(40),
                "bad",
                "/data",
                "c".repeat(MAX_TORRENT_CATEGORY_BYTES + 1),
            ],
        )
        .unwrap();
        assert!(list_torrent_labels_page(&conn, 0, 1).is_err());
    }

    #[test]
    fn label_page_rejects_too_many_tag_items() {
        let conn = setup();
        let tags = (0..=MAX_TORRENT_LABEL_RESULT_ITEMS)
            .map(|index| format!("tag-{index}"))
            .collect::<Vec<_>>();
        conn.execute(
            "INSERT INTO torrents
             (info_hash, name, total_length, piece_length, piece_count, is_private,
              save_path, category, tags, state, added_at, completed_at,
              uploaded, downloaded, amount_left, ratio, trackers)
             VALUES (?1, ?2, 1, 1, 1, 0, ?3, NULL, ?4, 'paused', 1, NULL, 0, 0, 1, 0, '[]')",
            params![
                "c".repeat(40),
                "bad",
                "/data",
                serde_json::to_string(&tags).unwrap(),
            ],
        )
        .unwrap();

        assert!(list_torrent_labels_page(&conn, 0, 1).is_err());
    }

    #[test]
    fn list_torrent_privacy_for_hashes_returns_only_requested_projection() {
        let conn = setup();
        let mut private = sample();
        private.is_private = true;
        let mut public = sample();
        public.info_hash = "b".repeat(40);
        public.name = "other.torrent".into();
        upsert(&conn, &private).unwrap();
        upsert(&conn, &public).unwrap();

        let projection =
            list_torrent_privacy_for_hashes(&conn, &[private.info_hash.clone(), "missing".into()])
                .unwrap();
        assert_eq!(projection, vec![(private.info_hash, true)]);
    }

    #[test]
    fn corrupt_serialized_labels_fail_closed() {
        let conn = setup();
        let row = sample();
        upsert(&conn, &row).unwrap();
        conn.execute("UPDATE torrents SET tags='not-json'", [])
            .unwrap();

        assert!(get(&conn, &row.info_hash).is_err());
        assert!(list_all(&conn).is_err());
    }
}
