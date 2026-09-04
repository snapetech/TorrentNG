use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};
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
            .optional()?;
        let mut groups: Vec<RatioGroup> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        groups.sort_by_key(|a| a.name.to_lowercase());
        Ok(groups)
    }

    pub fn upsert_ratio_group(&self, group: RatioGroup) -> Result<Vec<RatioGroup>> {
        let name = group.name.clone();
        self.update_ratio_groups(|groups| {
            groups.retain(|existing| existing.name != name);
            groups.push(group);
        })
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
            clauses.push(format!(
                "instr(lower(tracker_url), lower(?{})) > 0",
                args.len() + 1
            ));
            args.push(tracker.clone());
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
        self.update_ratio_groups(|groups| {
            groups.retain(|existing| existing.name != name);
        })
    }

    fn update_ratio_groups<F>(&self, update: F) -> Result<Vec<RatioGroup>>
    where
        F: FnOnce(&mut Vec<RatioGroup>),
    {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut groups: Vec<RatioGroup> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        update(&mut groups);
        groups.sort_by_key(|group| group.name.to_lowercase());
        let raw = serde_json::to_string(&groups)?;
        write_ratio_groups(&tx, &raw)?;
        tx.commit()?;
        Ok(groups)
    }
}

fn write_ratio_groups(tx: &Transaction<'_>, raw: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO kv(key, value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![KEY, raw],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn concurrent_ratio_group_updates_do_not_lose_groups() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let start = Arc::new(std::sync::Barrier::new(16));

        std::thread::scope(|scope| {
            for index in 0..16 {
                let db = db.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    db.upsert_ratio_group(RatioGroup {
                        name: format!("Group {index}"),
                        ratio_limit: 2.0,
                        seeding_time_limit: -1,
                        category: None,
                        tracker: None,
                        enabled: true,
                    })
                    .unwrap();
                });
            }
        });

        assert_eq!(db.list_ratio_groups().unwrap().len(), 16);
    }
}
