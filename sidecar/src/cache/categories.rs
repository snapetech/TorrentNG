use anyhow::Result;
use rusqlite::params;

use super::db::{allocate_revision, canonical_hash, prune_removed_tombstones, Db};

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
        let conn = self.0.lock().expect("db");
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
                    COALESCE(counts.torrent_count, 0) AS torrent_count
             FROM names
             LEFT JOIN categories ON categories.name = names.name
             LEFT JOIN counts ON counts.name = names.name
             ORDER BY names.name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Category {
                    name: r.get(0)?,
                    save_path: r.get(1)?,
                    torrent_count: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_category(&self, name: &str, save_path: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
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
        let mut conn = self.0.lock().expect("db");
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
        let conn = self.0.lock().expect("db");
        let mut stmt = conn.prepare("SELECT save_path FROM categories WHERE name=?1")?;
        let mut rows = stmt.query(params![name])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    // --- Tags ---

    pub fn list_tags(&self) -> Result<Vec<String>> {
        let conn = self.0.lock().expect("db");
        let mut stmt = conn.prepare("SELECT name FROM tags ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn ensure_tag(&self, name: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![name])?;
        let revision = allocate_revision(&tx)?;
        prune_removed_tombstones(&tx, revision)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
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
        let conn = self.0.lock().expect("db");
        let mut stmt =
            conn.prepare("SELECT tag FROM torrent_tags WHERE hash=?1 COLLATE NOCASE ORDER BY tag")?;
        let rows = stmt
            .query_map(params![hash], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn add_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
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
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
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
        let mut conn = self.0.lock().expect("db");
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
        let mut conn = self.0.lock().expect("db");
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
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
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
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
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
        let mut conn = self.0.lock().expect("db");
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
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let hash = canonical_existing_hash(&tx, hash)?;
        let revision = allocate_revision(&tx)?;
        let changed = tx.execute(
            "UPDATE torrents SET state=?1, is_active=?2, is_open=?3, revision=?4, updated_at=(
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
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let revision = allocate_revision(&tx)?;
        {
            let mut stmt = tx.prepare(
                "UPDATE torrents SET state=?1, is_active=?2, is_open=?3, revision=?4, updated_at=(
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
