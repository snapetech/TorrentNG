/// Torrent record stored in the `torrents` table.
use rusqlite::{params, types::Type, Connection, Row};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::DbError;

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

impl TorrentRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let tags_json: String = row.get(8)?;
        let tags: Vec<String> = decode_json_column(&tags_json, 8)?;
        let trackers_json: String = row.get(16)?;
        let trackers: Vec<String> = decode_json_column(&trackers_json, 16)?;
        Ok(TorrentRow {
            info_hash: row.get(0)?,
            name: row.get(1)?,
            total_length: row.get(2)?,
            piece_length: row.get(3)?,
            piece_count: row.get(4)?,
            is_private: row.get::<_, i64>(5)? != 0,
            save_path: row.get(6)?,
            category: row.get(7)?,
            tags,
            state: row.get(9)?,
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

fn decode_json_column<T: DeserializeOwned>(value: &str, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(error))
    })
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

pub fn list_categories(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare("SELECT name FROM torrent_categories ORDER BY name ASC")?;
    let categories = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(categories)
}

/// Return the durable category definitions, including their optional default
/// save paths.  Torrent labels are deliberately kept separate from these
/// definitions: a category can exist before any torrent uses it.
pub fn list_category_definitions(
    conn: &Connection,
) -> Result<Vec<(String, Option<String>)>, DbError> {
    let mut stmt =
        conn.prepare("SELECT name, save_path FROM torrent_categories ORDER BY name ASC")?;
    let categories = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, Option<String>)>>>()?;
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
    let mut stmt = conn.prepare(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, amount_left, ratio, trackers
         FROM torrents ORDER BY added_at DESC",
    )?;
    let rows = stmt
        .query_map([], TorrentRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn list_by_state(conn: &Connection, state: &str) -> Result<Vec<TorrentRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT info_hash, name, total_length, piece_length, piece_count, is_private,
                save_path, category, tags, state, added_at, completed_at,
                uploaded, downloaded, amount_left, ratio, trackers
         FROM torrents WHERE state = ?1 ORDER BY added_at DESC",
    )?;
    let rows = stmt
        .query_map(params![state], TorrentRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
