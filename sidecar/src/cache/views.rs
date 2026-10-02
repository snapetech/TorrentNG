use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{
    db::{kv_value_bounded, Db, MAX_KV_VALUE_BYTES},
    ListParams,
};

const KEY: &str = "saved_views";
const MAX_VIEWS: usize = 64;
const MAX_SAVED_VIEW_TEXT_BYTES: usize = 64 * 1024;

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
        let conn = self.read()?;
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut views: Vec<SavedView> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        if views.len() > MAX_VIEWS {
            bail!("saved views exceed the maximum of {MAX_VIEWS}");
        }
        for view in &views {
            validate_saved_view(view)?;
        }
        views.sort_by_key(|a| a.name.to_lowercase());
        Ok(views)
    }

    pub fn upsert_saved_view(&self, mut view: SavedView) -> Result<Vec<SavedView>> {
        if view.id.trim().is_empty() {
            view.id = uuid::Uuid::new_v4().to_string();
        }
        validate_saved_view(&view)?;
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
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let raw = kv_value_bounded(&tx, KEY, MAX_KV_VALUE_BYTES, "saved views")?;
        let mut views: Vec<SavedView> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        for view in &views {
            validate_saved_view(view)?;
        }
        update(&mut views);
        if views.len() > MAX_VIEWS {
            bail!("saved views exceed the maximum of {MAX_VIEWS}");
        }
        views.sort_by_key(|view| view.name.to_lowercase());
        for view in &views {
            validate_saved_view(view)?;
        }
        let raw = serde_json::to_string(&views)?;
        write_saved_views(&tx, &raw)?;
        tx.commit()?;
        Ok(views)
    }
}

pub(crate) fn validate_saved_view(view: &SavedView) -> Result<()> {
    validate_saved_view_text("saved view id", &view.id)?;
    validate_saved_view_text("saved view name", &view.name)?;
    for (label, value) in [
        ("saved view filter", view.params.filter.as_deref()),
        ("saved view status", view.params.status.as_deref()),
        ("saved view category", view.params.category.as_deref()),
        ("saved view tag", view.params.tag.as_deref()),
        ("saved view tracker", view.params.tracker.as_deref()),
        ("saved view media type", view.params.media_type.as_deref()),
        ("saved view sort", view.params.sort.as_deref()),
        ("saved view direction", view.params.dir.as_deref()),
    ] {
        if let Some(value) = value {
            validate_saved_view_text(label, value)?;
        }
    }
    Ok(())
}

fn validate_saved_view_text(label: &str, value: &str) -> Result<()> {
    if value.len() > MAX_SAVED_VIEW_TEXT_BYTES {
        bail!("{label} exceeds the maximum of {MAX_SAVED_VIEW_TEXT_BYTES} bytes");
    }
    Ok(())
}

fn write_saved_views(tx: &Transaction<'_>, raw: &str) -> Result<()> {
    if raw.len() > MAX_KV_VALUE_BYTES {
        bail!("stored saved views exceed the maximum of {MAX_KV_VALUE_BYTES} bytes");
    }
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

    #[test]
    fn saved_view_listing_fails_closed_when_legacy_data_is_oversized() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let views = (0..=MAX_VIEWS)
            .map(|index| SavedView {
                id: format!("view-{index}"),
                name: format!("View {index}"),
                params: SavedViewParams::default(),
            })
            .collect::<Vec<_>>();
        db.set_kv(KEY, &serde_json::to_string(&views).unwrap())
            .unwrap();

        assert!(db.list_saved_views().is_err());
    }

    #[test]
    fn saved_view_reads_reject_oversized_legacy_blobs_before_json_conversion() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let oversized = "x".repeat(MAX_KV_VALUE_BYTES + 1);
        db.conn()
            .expect("healthy test writer")
            .execute(
                "INSERT INTO kv(key, value) VALUES(?1, ?2)",
                params![KEY, oversized],
            )
            .unwrap();

        assert!(db.list_saved_views().is_err());
    }

    #[test]
    fn saved_view_text_limits_fail_closed() {
        let mut view = SavedView {
            id: "view".to_owned(),
            name: "View".to_owned(),
            params: SavedViewParams::default(),
        };
        assert!(validate_saved_view(&view).is_ok());
        view.params.filter = Some("x".repeat(MAX_SAVED_VIEW_TEXT_BYTES + 1));
        assert!(validate_saved_view(&view).is_err());
    }

    #[test]
    fn saved_view_updates_reject_overflow_instead_of_dropping_entries() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        for index in 0..MAX_VIEWS {
            db.upsert_saved_view(SavedView {
                id: format!("view-{index}"),
                name: format!("View {index:03}"),
                params: SavedViewParams::default(),
            })
            .unwrap();
        }

        let error = db
            .upsert_saved_view(SavedView {
                id: "overflow".to_owned(),
                name: "View overflow".to_owned(),
                params: SavedViewParams::default(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("saved views exceed"));
        let views = db.list_saved_views().unwrap();
        assert_eq!(views.len(), MAX_VIEWS);
        assert!(!views.iter().any(|view| view.id == "overflow"));
    }
}
