use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{db::Db, ListParams};

const KEY: &str = "saved_views";
const MAX_VIEWS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedView {
    pub id: String,
    pub name: String,
    pub params: SavedViewParams,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SavedViewParams {
    pub filter: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub tracker: Option<String>,
    pub media_type: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
}

impl From<SavedViewParams> for ListParams {
    fn from(params: SavedViewParams) -> Self {
        Self {
            filter: params.filter,
            status: params.status,
            category: params.category,
            tag: params.tag,
            tracker: params.tracker,
            media_type: params.media_type,
            sort: params.sort,
            dir: params.dir,
            limit: None,
            offset: None,
        }
    }
}

impl Db {
    pub fn list_saved_views(&self) -> Result<Vec<SavedView>> {
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut views: Vec<SavedView> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        views.sort_by_key(|a| a.name.to_lowercase());
        Ok(views)
    }

    pub fn upsert_saved_view(&self, mut view: SavedView) -> Result<Vec<SavedView>> {
        if view.id.trim().is_empty() {
            view.id = uuid::Uuid::new_v4().to_string();
        }
        let id = view.id.clone();
        self.update_saved_views(|views| {
            views.retain(|existing| existing.id != id && existing.name != view.name);
            views.push(view);
        })
    }

    pub fn delete_saved_view(&self, id: &str) -> Result<Vec<SavedView>> {
        self.update_saved_views(|views| {
            views.retain(|existing| existing.id != id);
        })
    }

    fn update_saved_views<F>(&self, update: F) -> Result<Vec<SavedView>>
    where
        F: FnOnce(&mut Vec<SavedView>),
    {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut views: Vec<SavedView> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        update(&mut views);
        views.sort_by_key(|view| view.name.to_lowercase());
        views.truncate(MAX_VIEWS);
        let raw = serde_json::to_string(&views)?;
        write_saved_views(&tx, &raw)?;
        tx.commit()?;
        Ok(views)
    }
}

fn write_saved_views(tx: &Transaction<'_>, raw: &str) -> Result<()> {
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
    fn concurrent_saved_view_updates_do_not_lose_views() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let start = Arc::new(std::sync::Barrier::new(16));

        std::thread::scope(|scope| {
            for index in 0..16 {
                let db = db.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    db.upsert_saved_view(SavedView {
                        id: format!("view-{index}"),
                        name: format!("View {index}"),
                        params: SavedViewParams::default(),
                    })
                    .unwrap();
                });
            }
        });

        assert_eq!(db.list_saved_views().unwrap().len(), 16);
    }
}
