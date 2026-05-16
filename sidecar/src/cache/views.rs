use anyhow::Result;
use rusqlite::params;
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
            .ok();
        let mut views: Vec<SavedView> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        views.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(views)
    }

    pub fn upsert_saved_view(&self, mut view: SavedView) -> Result<Vec<SavedView>> {
        if view.id.trim().is_empty() {
            view.id = uuid::Uuid::new_v4().to_string();
        }
        let mut views = self.list_saved_views()?;
        views.retain(|existing| existing.id != view.id && existing.name != view.name);
        views.push(view);
        views.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        if views.len() > MAX_VIEWS {
            views.truncate(MAX_VIEWS);
        }
        self.save_saved_views(&views)?;
        Ok(views)
    }

    pub fn delete_saved_view(&self, id: &str) -> Result<Vec<SavedView>> {
        let mut views = self.list_saved_views()?;
        views.retain(|existing| existing.id != id);
        self.save_saved_views(&views)?;
        Ok(views)
    }

    fn save_saved_views(&self, views: &[SavedView]) -> Result<()> {
        let raw = serde_json::to_string(views)?;
        self.0.lock().expect("db").execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![KEY, raw],
        )?;
        Ok(())
    }
}
