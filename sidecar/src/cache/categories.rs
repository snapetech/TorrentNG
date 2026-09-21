use anyhow::{anyhow, bail, Result};
use rusqlite::{params, params_from_iter, OptionalExtension, Transaction};
use std::{collections::BTreeSet, error::Error, fmt};

use super::db::{allocate_revision, canonical_hash, prune_removed_tombstones, Db};

pub(crate) const MAX_CACHED_LABEL_DICTIONARY_ITEMS: usize = 16_384;
pub(crate) const MAX_CACHED_LABEL_DICTIONARY_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CACHED_LABEL_NAME_BYTES: usize = 256;
const MAX_CACHED_CATEGORY_PATH_BYTES: usize = 4_096;
pub(crate) const MAX_CACHED_TAGS_PER_MUTATION: usize = 1_024;
pub(crate) const MAX_CACHED_TAG_MUTATION_BYTES: usize = 512 * 1024;
pub(crate) const MAX_CACHED_TAG_ASSIGNMENTS: usize = 1_000_000;
pub(crate) const MAX_CACHED_TAG_ASSIGNMENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct CategoryTagCapacityError {
    resource: &'static str,
}

impl CategoryTagCapacityError {
    pub(crate) fn new(resource: &'static str) -> Self {
        Self { resource }
    }
}

impl fmt::Display for CategoryTagCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cached {} metadata exceeds its bounded capacity",
            self.resource
        )
    }
}

impl Error for CategoryTagCapacityError {}

pub(crate) fn check_tag_capacity(tx: &Transaction<'_>, tags: &[&str]) -> Result<()> {
    if tags.len() > MAX_CACHED_TAGS_PER_MUTATION {
        bail!("tag mutation exceeds the {MAX_CACHED_TAGS_PER_MUTATION}-item limit");
    }
    let mutation_bytes = tags
        .iter()
        .fold(0usize, |total, tag| total.saturating_add(tag.len()));
    if mutation_bytes > MAX_CACHED_TAG_MUTATION_BYTES {
        bail!("tag mutation exceeds the byte limit");
    }

    let mut distinct = BTreeSet::new();
    for tag in tags {
        if tag.len() > MAX_CACHED_LABEL_NAME_BYTES {
            bail!("tag name exceeds the {MAX_CACHED_LABEL_NAME_BYTES}-byte limit");
        }
        if tag.trim().is_empty() {
            bail!("tag name must not be empty");
        }
        distinct.insert(*tag);
    }

    let mut missing = distinct.clone();
    for chunk in distinct.iter().copied().collect::<Vec<_>>().chunks(400) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT name FROM tags WHERE name IN ({placeholders})");
        let mut statement = tx.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(chunk.iter().copied()))?;
        while let Some(row) = rows.next()? {
            let found: String = row.get(0)?;
            missing.remove(found.as_str());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }

    let (current_items, current_bytes): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(name AS BLOB))), 0) FROM tags",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let new_bytes = missing
        .iter()
        .fold(0usize, |total, tag| total.saturating_add(tag.len()));
    let next_items = (current_items.max(0) as usize).saturating_add(missing.len());
    let next_bytes = (current_bytes.max(0) as usize).saturating_add(new_bytes);
    if next_items > MAX_CACHED_LABEL_DICTIONARY_ITEMS
        || next_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES
    {
        return Err(anyhow!(CategoryTagCapacityError::new("tag")));
    }
    Ok(())
}

pub(super) fn check_torrent_tag_capacity(
    tx: &Transaction<'_>,
    hash: &str,
    tags: &[&str],
    append: bool,
) -> Result<()> {
    check_torrent_tags_capacity(tx, &[hash], tags, append)
}

fn check_torrent_tags_capacity(
    tx: &Transaction<'_>,
    hashes: &[&str],
    tags: &[&str],
    append: bool,
) -> Result<()> {
    let distinct = tags.iter().copied().collect::<BTreeSet<_>>();
    if distinct.len() > MAX_CACHED_TAGS_PER_MUTATION {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "per-torrent tag list"
        )));
    }
    if hashes.is_empty() {
        return Ok(());
    }
    let (global_count, global_bytes): (i64, i64) = tx.query_row(
        "SELECT assignment_count, assignment_bytes
         FROM torrent_tag_totals WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (global_count, global_bytes) =
        match (usize::try_from(global_count), usize::try_from(global_bytes)) {
            (Ok(global_count), Ok(global_bytes)) => (global_count, global_bytes),
            _ => {
                return Err(anyhow!(CategoryTagCapacityError::new(
                    "torrent tag assignments"
                )))
            }
        };
    let distinct_values = distinct.iter().copied().collect::<Vec<_>>();
    let mut seen_hashes = BTreeSet::new();
    let mut positive_count_growth = 0usize;
    let mut positive_byte_growth = 0usize;

    for hash in hashes.iter().copied() {
        if !seen_hashes.insert(hash) {
            continue;
        }
        let unique_hash_count = seen_hashes.len();
        let (current_count, current_bytes): (i64, i64) = tx
            .query_row(
                "SELECT assignment_count, assignment_bytes
                 FROM torrent_tag_stats WHERE hash=?1",
                params![hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or_default();
        let (current_count, current_bytes) = match (
            usize::try_from(current_count),
            usize::try_from(current_bytes),
        ) {
            (Ok(count), Ok(bytes)) if count <= global_count && bytes <= global_bytes => {
                (count, bytes)
            }
            _ => {
                return Err(anyhow!(CategoryTagCapacityError::new(
                    "torrent tag assignments"
                )))
            }
        };

        let (removed_for_hash, removed_bytes_for_hash, added_tags) = if append {
            let mut new_links = distinct.clone();
            for chunk in distinct_values.chunks(399) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT tag FROM torrent_tags WHERE hash=?1 AND tag IN ({placeholders})"
                );
                let mut statement = tx.prepare(&sql)?;
                let mut rows = statement.query(params_from_iter(
                    std::iter::once(hash).chain(chunk.iter().copied()),
                ))?;
                while let Some(row) = rows.next()? {
                    let found: String = row.get(0)?;
                    new_links.remove(found.as_str());
                }
            }
            (0, 0, new_links)
        } else {
            (current_count, current_bytes, distinct.clone())
        };
        let next_torrent_count = if append {
            current_count.saturating_add(added_tags.len())
        } else {
            added_tags.len()
        };
        if next_torrent_count > MAX_CACHED_TAGS_PER_MUTATION {
            return Err(anyhow!(CategoryTagCapacityError::new(
                "per-torrent tag list"
            )));
        }

        let added_bytes_for_hash = added_tags
            .iter()
            .fold(0usize, |total, tag| total.saturating_add(tag.len()));
        positive_count_growth =
            positive_count_growth.saturating_add(added_tags.len().saturating_sub(removed_for_hash));
        positive_byte_growth = positive_byte_growth
            .saturating_add(added_bytes_for_hash.saturating_sub(removed_bytes_for_hash));
        if append || unique_hash_count > 1 || positive_count_growth > 0 || positive_byte_growth > 0
        {
            // qBittorrent mutates a multi-hash selection one torrent at a
            // time. Budget only gross positive growth so any successful
            // subset remains within the cache bound regardless of order.
            check_tag_assignment_growth(
                global_count,
                global_bytes,
                positive_count_growth,
                positive_byte_growth,
            )?;
        }
    }
    Ok(())
}

fn check_tag_assignment_growth(
    current_count: usize,
    current_bytes: usize,
    added_count: usize,
    added_bytes: usize,
) -> Result<()> {
    let count_over_capacity = if current_count > MAX_CACHED_TAG_ASSIGNMENTS {
        added_count > 0
    } else {
        added_count > MAX_CACHED_TAG_ASSIGNMENTS - current_count
    };
    let bytes_over_capacity = if current_bytes > MAX_CACHED_TAG_ASSIGNMENT_BYTES {
        added_bytes > 0
    } else {
        added_bytes > MAX_CACHED_TAG_ASSIGNMENT_BYTES - current_bytes
    };
    if count_over_capacity || bytes_over_capacity {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "torrent tag assignments"
        )));
    }
    Ok(())
}

fn check_tag_assignment_totals(
    current_count: usize,
    current_bytes: usize,
    removed_count: usize,
    removed_bytes: usize,
    added_count: usize,
    added_bytes: usize,
) -> Result<(usize, usize)> {
    let Some(next_count) = current_count
        .checked_sub(removed_count)
        .and_then(|count| count.checked_add(added_count))
    else {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "torrent tag assignments"
        )));
    };
    let Some(next_bytes) = current_bytes
        .checked_sub(removed_bytes)
        .and_then(|bytes| bytes.checked_add(added_bytes))
    else {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "torrent tag assignments"
        )));
    };
    if next_count > MAX_CACHED_TAG_ASSIGNMENTS || next_bytes > MAX_CACHED_TAG_ASSIGNMENT_BYTES {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "torrent tag assignments"
        )));
    }
    Ok((next_count, next_bytes))
}

pub(super) fn check_tag_assignment_projection_capacity(conn: &rusqlite::Connection) -> Result<()> {
    let totals = conn
        .query_row(
            "SELECT assignment_count, assignment_bytes
             FROM torrent_tag_totals WHERE singleton=1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let Some((count, bytes)) = totals else {
        return Err(anyhow!(CategoryTagCapacityError::new(
            "torrent tag assignments"
        )));
    };
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let bytes = usize::try_from(bytes).unwrap_or(usize::MAX);
    check_tag_assignment_totals(count, bytes, 0, 0, 0, 0).map(|_| ())
}

pub(crate) fn check_category_capacity(
    tx: &Transaction<'_>,
    replaced_hash: Option<&str>,
    name: &str,
) -> Result<()> {
    if name.len() > MAX_CACHED_LABEL_NAME_BYTES {
        bail!("category name exceeds the {MAX_CACHED_LABEL_NAME_BYTES}-byte limit");
    }
    if name.is_empty() || category_exists(tx, replaced_hash, name)? {
        return Ok(());
    }

    let (current_items, current_bytes) = category_projection_totals(tx, replaced_hash)?;
    let next_items = current_items.saturating_add(1);
    let next_bytes = current_bytes.saturating_add(name.len());
    if next_items > MAX_CACHED_LABEL_DICTIONARY_ITEMS
        || next_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES
    {
        return Err(anyhow!(CategoryTagCapacityError::new("category")));
    }
    Ok(())
}

fn check_category_definition_capacity(
    tx: &Transaction<'_>,
    name: &str,
    save_path: &str,
) -> Result<()> {
    if name.len() > MAX_CACHED_LABEL_NAME_BYTES {
        bail!("category name exceeds the {MAX_CACHED_LABEL_NAME_BYTES}-byte limit");
    }
    if save_path.len() > MAX_CACHED_CATEGORY_PATH_BYTES {
        bail!("category save path exceeds the {MAX_CACHED_CATEGORY_PATH_BYTES}-byte limit");
    }

    let (current_items, current_bytes) = category_projection_totals(tx, None)?;
    let old_path_bytes = tx
        .query_row(
            "SELECT length(CAST(save_path AS BLOB)) FROM categories WHERE name=?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if old_path_bytes
        .is_some_and(|bytes| bytes < 0 || bytes as usize > MAX_CACHED_CATEGORY_PATH_BYTES)
    {
        return Err(anyhow!(CategoryTagCapacityError::new("category")));
    }
    let exists = category_exists(tx, None, name)?;
    let next_items = current_items.saturating_add(usize::from(!exists));
    let next_bytes = if let Some(old_path_bytes) = old_path_bytes {
        current_bytes
            .saturating_sub(old_path_bytes.max(0) as usize)
            .saturating_add(save_path.len())
    } else if exists {
        current_bytes.saturating_add(save_path.len())
    } else {
        current_bytes.saturating_add(name.len().saturating_add(save_path.len()))
    };
    if next_items > MAX_CACHED_LABEL_DICTIONARY_ITEMS
        || next_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES
    {
        return Err(anyhow!(CategoryTagCapacityError::new("category")));
    }
    Ok(())
}

fn category_exists(tx: &Transaction<'_>, replaced_hash: Option<&str>, name: &str) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM categories WHERE name=?1)
             OR EXISTS(SELECT 1 FROM torrents
                       WHERE category=?1 AND (?2='' OR hash!=?2))",
        params![name, replaced_hash.unwrap_or("")],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn category_projection_totals(
    tx: &Transaction<'_>,
    replaced_hash: Option<&str>,
) -> Result<(usize, usize)> {
    let (items, bytes): (i64, i64) = tx.query_row(
        "WITH names(name) AS (
             SELECT name FROM categories WHERE name!=''
             UNION
             SELECT DISTINCT category FROM torrents
             WHERE category!='' AND (?1='' OR hash!=?1)
         )
         SELECT COUNT(*), COALESCE(SUM(
             length(CAST(names.name AS BLOB)) +
             COALESCE(length(CAST(categories.save_path AS BLOB)), 0)
         ), 0)
         FROM names LEFT JOIN categories ON categories.name=names.name",
        params![replaced_hash.unwrap_or("")],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((items.max(0) as usize, bytes.max(0) as usize))
}

pub(crate) fn ensure_category_name_capacity(db: &Db, name: &str) -> Result<()> {
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    check_category_capacity(&tx, None, name)
}

pub(crate) fn ensure_tag_name_capacity(db: &Db, tags: &[&str]) -> Result<()> {
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    check_tag_capacity(&tx, tags)
}

pub(crate) fn ensure_torrent_tag_capacity(
    db: &Db,
    hash: &str,
    tags: &[&str],
    append: bool,
) -> Result<()> {
    ensure_torrent_tag_capacities(db, &[hash.to_owned()], tags, append)
}

pub(crate) fn ensure_torrent_tag_capacities(
    db: &Db,
    hashes: &[String],
    tags: &[&str],
    append: bool,
) -> Result<()> {
    let conn = db.conn()?;
    let tx = conn.unchecked_transaction()?;
    check_tag_capacity(&tx, tags)?;
    let mut canonical_hashes = Vec::with_capacity(hashes.len());
    for hash in hashes {
        let canonical_hash: String = tx
            .query_row(
                "SELECT hash FROM torrents WHERE hash=?1 COLLATE NOCASE",
                params![hash],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("torrent not found in cache"))?;
        canonical_hashes.push(canonical_hash);
    }
    let hash_refs = canonical_hashes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    check_torrent_tags_capacity(&tx, &hash_refs, tags, append)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Category {
    pub name: String,
    pub save_path: String,
    pub torrent_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    pub name: String,
}

impl Db {
    // --- Categories ---

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "WITH names AS (
                SELECT name FROM categories WHERE name != ''
                UNION
                SELECT DISTINCT category AS name FROM torrents WHERE category != ''
             ),
             counts AS (
                SELECT category AS name, COUNT(*) AS torrent_count
                FROM torrents
                WHERE category != ''
                GROUP BY category
             )
             SELECT names.name,
                    COALESCE(categories.save_path, '') AS save_path,
                    COALESCE(counts.torrent_count, 0) AS torrent_count,
                    length(CAST(names.name AS BLOB)),
                    length(CAST(COALESCE(categories.save_path, '') AS BLOB))
             FROM names
             LEFT JOIN categories ON categories.name = names.name
             LEFT JOIN counts ON counts.name = names.name
             ORDER BY names.name COLLATE NOCASE
             LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![(MAX_CACHED_LABEL_DICTIONARY_ITEMS + 1) as i64])?;
        let mut categories = Vec::new();
        let mut total_bytes = 0usize;
        while let Some(row) = rows.next()? {
            if categories.len() >= MAX_CACHED_LABEL_DICTIONARY_ITEMS {
                return Err(anyhow!(CategoryTagCapacityError::new("category")));
            }
            let name_bytes: i64 = row.get(3)?;
            let path_bytes: i64 = row.get(4)?;
            if name_bytes < 0
                || name_bytes as usize > MAX_CACHED_LABEL_NAME_BYTES
                || path_bytes < 0
                || path_bytes as usize > MAX_CACHED_CATEGORY_PATH_BYTES
            {
                return Err(anyhow!(CategoryTagCapacityError::new("category")));
            }
            let category = Category {
                name: row.get(0)?,
                save_path: row.get(1)?,
                torrent_count: row.get(2)?,
            };
            let row_bytes = category.name.len().saturating_add(category.save_path.len());
            total_bytes = total_bytes.saturating_add(row_bytes);
            if row_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES
                || total_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES
            {
                return Err(anyhow!(CategoryTagCapacityError::new("category")));
            }
            categories.push(category);
        }
        Ok(categories)
    }

    pub fn upsert_category(&self, name: &str, save_path: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        check_category_definition_capacity(&tx, name, save_path)?;
        tx.execute(
            "INSERT INTO categories(name, save_path) VALUES(?1,?2)
             ON CONFLICT(name) DO UPDATE SET save_path=excluded.save_path",
            params![name, save_path],
        )?;
        let revision = allocate_revision(&tx)?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_category(&self, name: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM categories WHERE name=?1", params![name])?;
        let revision = allocate_revision(&tx)?;
        tx.execute(
            "UPDATE torrents SET category='', revision=?1, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE category=?2",
            params![revision, name],
        )?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_category_save_path(&self, name: &str) -> Result<Option<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT save_path, length(CAST(save_path AS BLOB)) FROM categories WHERE name=?1",
        )?;
        let mut rows = stmt.query(params![name])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => {
                let path_bytes: i64 = row.get(1)?;
                if path_bytes < 0 || path_bytes as usize > MAX_CACHED_CATEGORY_PATH_BYTES {
                    return Err(anyhow!(CategoryTagCapacityError::new("category")));
                }
                Ok(Some(row.get(0)?))
            }
        }
    }

    // --- Tags ---

    pub fn list_tags(&self) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT name, length(CAST(name AS BLOB)) FROM tags ORDER BY name LIMIT ?1")?;
        let mut rows = stmt.query(params![(MAX_CACHED_LABEL_DICTIONARY_ITEMS + 1) as i64])?;
        let mut tags = Vec::new();
        let mut total_bytes = 0usize;
        while let Some(row) = rows.next()? {
            if tags.len() >= MAX_CACHED_LABEL_DICTIONARY_ITEMS {
                return Err(anyhow!(CategoryTagCapacityError::new("tag")));
            }
            let tag_bytes: i64 = row.get(1)?;
            if tag_bytes < 0 || tag_bytes as usize > MAX_CACHED_LABEL_NAME_BYTES {
                return Err(anyhow!(CategoryTagCapacityError::new("tag")));
            }
            total_bytes = total_bytes.saturating_add(tag_bytes as usize);
            if total_bytes > MAX_CACHED_LABEL_DICTIONARY_BYTES {
                return Err(anyhow!(CategoryTagCapacityError::new("tag")));
            }
            let tag: String = row.get(0)?;
            tags.push(tag);
        }
        Ok(tags)
    }

    pub fn ensure_tag(&self, name: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        check_tag_capacity(&tx, &[name])?;
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![name])?;
        let revision = allocate_revision(&tx)?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let revision = allocate_revision(&tx)?;
        tx.execute(
            "UPDATE torrents SET revision=?1, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             )
             WHERE EXISTS (SELECT 1 FROM torrent_tags tt WHERE tt.hash=torrents.hash AND tt.tag=?2)",
            params![revision, name],
        )?;
        tx.execute("DELETE FROM tags WHERE name=?1", params![name])?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    // --- Torrent tags ---

    pub fn get_torrent_tags(&self, hash: &str) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut stmt = conn.prepare(
            "SELECT tag, length(CAST(tag AS BLOB)) FROM torrent_tags
             WHERE hash=?1 COLLATE NOCASE ORDER BY tag LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![hash, (MAX_CACHED_TAGS_PER_MUTATION + 1) as i64])?;
        let mut tags = Vec::new();
        let mut total_bytes = 0usize;
        while let Some(row) = rows.next()? {
            if tags.len() >= MAX_CACHED_TAGS_PER_MUTATION {
                return Err(anyhow!(CategoryTagCapacityError::new(
                    "per-torrent tag list"
                )));
            }
            let tag_bytes: i64 = row.get(1)?;
            if tag_bytes < 0 || tag_bytes as usize > MAX_CACHED_LABEL_NAME_BYTES {
                return Err(anyhow!(CategoryTagCapacityError::new(
                    "per-torrent tag list"
                )));
            }
            total_bytes = total_bytes.saturating_add(tag_bytes as usize);
            if total_bytes > MAX_CACHED_TAG_MUTATION_BYTES {
                return Err(anyhow!(CategoryTagCapacityError::new(
                    "per-torrent tag list"
                )));
            }
            tags.push(row.get(0)?);
        }
        Ok(tags)
    }

    pub fn add_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        check_tag_capacity(&tx, &[tag])?;
        check_torrent_tag_capacity(&tx, &hash, &[tag], true)?;
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
        tx.execute(
            "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
            params![hash, tag],
        )?;
        touch_torrent(&tx, &hash)?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_torrent_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        if tags.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        check_tag_capacity(&tx, tags)?;
        check_torrent_tag_capacity(&tx, &hash, tags, true)?;
        for tag in tags {
            tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
            tx.execute(
                "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
                params![hash, tag],
            )?;
        }
        touch_torrent(&tx, &hash)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        tx.execute(
            "DELETE FROM torrent_tags WHERE hash=?1 AND tag=?2",
            params![hash, tag],
        )?;
        touch_torrent(&tx, &hash)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_torrent_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        if tags.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        for tag in tags {
            tx.execute(
                "DELETE FROM torrent_tags WHERE hash=?1 AND tag=?2",
                params![hash, tag],
            )?;
        }
        touch_torrent(&tx, &hash)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_torrent_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        check_tag_capacity(&tx, tags)?;
        check_torrent_tag_capacity(&tx, &hash, tags, false)?;
        tx.execute("DELETE FROM torrent_tags WHERE hash=?1", params![hash])?;
        for tag in tags {
            tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
            tx.execute(
                "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
                params![hash, tag],
            )?;
        }
        touch_torrent(&tx, &hash)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_torrent_category(&self, hash: &str, category: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        check_category_capacity(&tx, Some(&hash), category)?;
        let revision = allocate_revision(&tx)?;
        let changed = tx.execute(
            "UPDATE torrents SET category=?1, revision=?2, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?3",
            params![category, revision, hash],
        )?;
        if changed == 0 {
            return Err(anyhow::anyhow!("torrent {hash} not found"));
        }
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_torrent_location(&self, hash: &str, location: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        let revision = allocate_revision(&tx)?;
        let changed = tx.execute(
            "UPDATE torrents SET directory=?1, revision=?2, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?3",
            params![location, revision, hash],
        )?;
        if changed == 0 {
            return Err(anyhow::anyhow!("torrent {hash} not found"));
        }
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_torrent_runtime_state(
        &self,
        hash: &str,
        state: i64,
        is_active: bool,
        is_open: bool,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        let revision = allocate_revision(&tx)?;
        let changed = tx.execute(
            // A manual lifecycle action invalidates the previous rate sample.
            // Do not make old throughput look fresh merely because the state
            // projection updates its timestamp.
            "UPDATE torrents SET state=?1, is_active=?2, is_open=?3, down_rate=0, up_rate=0, revision=?4, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?5",
            params![state, is_active as i64, is_open as i64, revision, hash],
        )?;
        if changed == 0 {
            return Err(anyhow::anyhow!("torrent {hash} not found"));
        }
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    /// Same as calling set_torrent_runtime_state() once per row, but in one
    /// transaction instead of one autocommit (and, under WAL, one fsync) per
    /// row -- for a few thousand rows that's the difference between tens of
    /// seconds and under a second. Used after a bulk-optimized backend call
    /// (e.g. rTorrent's system.multicall) so the DB write doesn't become the
    /// new bottleneck once the RPC round trips are no longer it.
    pub fn set_torrent_runtime_state_many(
        &self,
        updates: &[(String, i64, bool, bool)],
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let revision = allocate_revision(&tx)?;
        {
            let mut stmt = tx.prepare(
                // Bulk lifecycle actions invalidate the previous rate sample
                // for the same reason as the single-row path above.
                "UPDATE torrents SET state=?1, is_active=?2, is_open=?3, down_rate=0, up_rate=0, revision=?4, updated_at=(
                    SELECT MAX(v) FROM (
                        SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                        UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                    )
                 ) WHERE hash=?5",
            )?;
            for (hash, state, is_active, is_open) in updates {
                let hash = canonical_existing_hash(&tx, hash)?;
                let changed = stmt.execute(params![
                    state,
                    *is_active as i64,
                    *is_open as i64,
                    revision,
                    hash
                ])?;
                if changed == 0 {
                    return Err(anyhow::anyhow!("torrent {hash} not found"));
                }
            }
        }
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }
}

fn touch_torrent(conn: &rusqlite::Connection, hash: &str) -> Result<()> {
    let hash = canonical_existing_hash(conn, hash)?;
    let revision = allocate_revision(conn)?;
    let changed = conn.execute(
        "UPDATE torrents SET revision=?1, updated_at=(
            SELECT MAX(v) FROM (
                SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
            )
         ) WHERE hash=?2",
        params![revision, hash],
    )?;
    if changed == 0 {
        return Err(anyhow::anyhow!("torrent {hash} not found"));
    }
    prune_removed_tombstones(conn, revision)?;
    Ok(())
}

fn canonical_existing_hash(conn: &rusqlite::Connection, hash: &str) -> Result<String> {
    canonical_hash(conn, hash)?.ok_or_else(|| anyhow::anyhow!("torrent {hash} not found"))
}

#[cfg(test)]
mod tests {
    use super::{
        check_tag_assignment_totals, CategoryTagCapacityError, MAX_CACHED_TAG_ASSIGNMENTS,
        MAX_CACHED_TAG_ASSIGNMENT_BYTES,
    };

    #[test]
    fn global_tag_assignment_capacity_checks_count_and_bytes_and_allows_replacements() {
        assert_eq!(
            check_tag_assignment_totals(
                MAX_CACHED_TAG_ASSIGNMENTS - 1,
                MAX_CACHED_TAG_ASSIGNMENT_BYTES - 3,
                0,
                0,
                1,
                3,
            )
            .unwrap(),
            (MAX_CACHED_TAG_ASSIGNMENTS, MAX_CACHED_TAG_ASSIGNMENT_BYTES)
        );

        let count_error =
            check_tag_assignment_totals(MAX_CACHED_TAG_ASSIGNMENTS, 0, 0, 0, 1, 1).unwrap_err();
        assert!(count_error
            .downcast_ref::<CategoryTagCapacityError>()
            .is_some());

        let bytes_error =
            check_tag_assignment_totals(0, MAX_CACHED_TAG_ASSIGNMENT_BYTES, 0, 0, 1, 1)
                .unwrap_err();
        assert!(bytes_error
            .downcast_ref::<CategoryTagCapacityError>()
            .is_some());

        assert_eq!(
            check_tag_assignment_totals(10, 100, 8, 80, 2, 20).unwrap(),
            (4, 40)
        );
    }
}
