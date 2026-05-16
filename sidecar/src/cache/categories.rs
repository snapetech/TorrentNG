use anyhow::Result;
use rusqlite::params;

use super::db::Db;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Category {
    pub name: String,
    pub save_path: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    pub name: String,
}

impl Db {
    // --- Categories ---

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let conn = self.0.lock().expect("db");
        let mut stmt = conn.prepare("SELECT name, save_path FROM categories ORDER BY name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Category {
                    name: r.get(0)?,
                    save_path: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_category(&self, name: &str, save_path: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "INSERT INTO categories(name, save_path) VALUES(?1,?2)
             ON CONFLICT(name) DO UPDATE SET save_path=excluded.save_path",
            params![name, save_path],
        )?;
        Ok(())
    }

    pub fn delete_category(&self, name: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM categories WHERE name=?1", params![name])?;
        tx.execute(
            "UPDATE torrents SET category='', updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE category=?1",
            params![name],
        )?;
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
        self.0
            .lock()
            .expect("db")
            .execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![name])?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE torrents SET updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             )
             WHERE EXISTS (SELECT 1 FROM torrent_tags tt WHERE tt.hash=torrents.hash AND tt.tag=?1)",
            params![name],
        )?;
        tx.execute("DELETE FROM tags WHERE name=?1", params![name])?;
        tx.commit()?;
        Ok(())
    }

    // --- Torrent tags ---

    pub fn get_torrent_tags(&self, hash: &str) -> Result<Vec<String>> {
        let conn = self.0.lock().expect("db");
        let mut stmt = conn.prepare("SELECT tag FROM torrent_tags WHERE hash=?1 ORDER BY tag")?;
        let rows = stmt
            .query_map(params![hash], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn add_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        let conn = self.0.lock().expect("db");
        conn.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
        conn.execute(
            "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
            params![hash, tag],
        )?;
        touch_torrent(&conn, hash)?;
        Ok(())
    }

    pub fn remove_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        let conn = self.0.lock().expect("db");
        conn.execute(
            "DELETE FROM torrent_tags WHERE hash=?1 AND tag=?2",
            params![hash, tag],
        )?;
        touch_torrent(&conn, hash)?;
        Ok(())
    }

    pub fn set_torrent_tags(&self, hash: &str, tags: &[&str]) -> Result<()> {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM torrent_tags WHERE hash=?1", params![hash])?;
        for tag in tags {
            tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![tag])?;
            tx.execute(
                "INSERT OR IGNORE INTO torrent_tags(hash, tag) VALUES(?1,?2)",
                params![hash, tag],
            )?;
        }
        tx.execute(
            "UPDATE torrents SET updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?1",
            params![hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_torrent_category(&self, hash: &str, category: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "UPDATE torrents SET category=?1, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?2",
            params![category, hash],
        )?;
        Ok(())
    }

    pub fn set_torrent_location(&self, hash: &str, location: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "UPDATE torrents SET directory=?1, updated_at=(
                SELECT MAX(v) FROM (
                    SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                    UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
                )
             ) WHERE hash=?2",
            params![location, hash],
        )?;
        Ok(())
    }
}

fn touch_torrent(conn: &rusqlite::Connection, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET updated_at=(
            SELECT MAX(v) FROM (
                SELECT CAST(strftime('%s','now') AS INTEGER) AS v
                UNION ALL SELECT COALESCE(MAX(updated_at), 0) + 1 FROM torrents
            )
         ) WHERE hash=?1",
        params![hash],
    )?;
    Ok(())
}
