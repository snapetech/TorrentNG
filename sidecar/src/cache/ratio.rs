use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::db::Db;

const KEY: &str = "ratio_groups";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RatioGroup {
    pub name: String,
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub category: Option<String>,
    pub tracker: Option<String>,
    pub enabled: bool,
}

impl Db {
    pub fn list_ratio_groups(&self) -> Result<Vec<RatioGroup>> {
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .ok();
        let mut groups: Vec<RatioGroup> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        groups.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(groups)
    }

    pub fn upsert_ratio_group(&self, group: RatioGroup) -> Result<Vec<RatioGroup>> {
        let mut groups = self.list_ratio_groups()?;
        groups.retain(|existing| existing.name != group.name);
        groups.push(group);
        groups.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_ratio_groups(&groups)?;
        Ok(groups)
    }

    pub fn get_ratio_group(&self, name: &str) -> Result<Option<RatioGroup>> {
        Ok(self
            .list_ratio_groups()?
            .into_iter()
            .find(|group| group.name == name))
    }

    pub fn ratio_group_hashes(&self, group: &RatioGroup) -> Result<Vec<String>> {
        let conn = self.0.lock().expect("db");
        let mut clauses = Vec::new();
        let mut args = Vec::new();

        if let Some(category) = &group.category {
            clauses.push(format!("category = ?{}", args.len() + 1));
            args.push(category.clone());
        }
        if let Some(tracker) = &group.tracker {
            clauses.push(format!("tracker_url LIKE ?{}", args.len() + 1));
            args.push(format!("%{tracker}%"));
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT hash FROM torrents{where_sql} ORDER BY name COLLATE NOCASE"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn delete_ratio_group(&self, name: &str) -> Result<Vec<RatioGroup>> {
        let mut groups = self.list_ratio_groups()?;
        groups.retain(|existing| existing.name != name);
        self.save_ratio_groups(&groups)?;
        Ok(groups)
    }

    fn save_ratio_groups(&self, groups: &[RatioGroup]) -> Result<()> {
        let raw = serde_json::to_string(groups)?;
        self.0.lock().expect("db").execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![KEY, raw],
        )?;
        Ok(())
    }
}
