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
        let rows = stmt.query_map([], |r| Ok(Category {
            name:      r.get(0)?,
            save_path: r.get(1)?,
        }))?.collect::<rusqlite::Result<Vec<_>>>()?;
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
        self.0.lock().expect("db").execute(
            "DELETE FROM categories WHERE name=?1", params![name],
        )?;
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
        let rows = stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn ensure_tag(&self, name: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "INSERT OR IGNORE INTO tags(name) VALUES(?1)", params![name],
        )?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "DELETE FROM tags WHERE name=?1", params![name],
        )?;
        Ok(())
    }

    // --- Torrent tags ---

    pub fn get_torrent_tags(&self, hash: &str) -> Result<Vec<String>> {
        let conn = self.0.lock().expect("db");
        let mut stmt = conn.prepare(
            "SELECT tag FROM torrent_tags WHERE hash=?1 ORDER BY tag"
        )?;
        let rows = stmt.query_map(params![hash], |r| r.get(0))?
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
        Ok(())
    }

    pub fn remove_torrent_tag(&self, hash: &str, tag: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "DELETE FROM torrent_tags WHERE hash=?1 AND tag=?2",
            params![hash, tag],
        )?;
        Ok(())
    }

    pub fn set_torrent_category(&self, hash: &str, category: &str) -> Result<()> {
        self.0.lock().expect("db").execute(
            "UPDATE torrents SET category=?1 WHERE hash=?2",
            params![category, hash],
        )?;
        Ok(())
    }
}
