use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageRootRow {
    pub root_id: String,
    pub path: String,
    pub profile: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MountRow {
    pub mount_id: String,
    pub path: String,
    pub fs_type: Option<String>,
    pub device: Option<String>,
    pub queue_depth: i64,
    pub read_concurrency: i64,
    pub write_concurrency: i64,
    pub updated_at: i64,
}

impl StorageRootRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(StorageRootRow {
            root_id: row.get(0)?,
            path: row.get(1)?,
            profile: row.get(2)?,
            created_at: row.get(3)?,
        })
    }
}

impl MountRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(MountRow {
            mount_id: row.get(0)?,
            path: row.get(1)?,
            fs_type: row.get(2)?,
            device: row.get(3)?,
            queue_depth: row.get(4)?,
            read_concurrency: row.get(5)?,
            write_concurrency: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

pub fn upsert_storage_root(conn: &Connection, row: &StorageRootRow) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO storage_roots (root_id, path, profile, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(root_id) DO UPDATE SET
            path=excluded.path,
            profile=excluded.profile",
        params![row.root_id, row.path, row.profile, row.created_at],
    )?;
    Ok(())
}

pub fn get_storage_root(conn: &Connection, root_id: &str) -> Result<StorageRootRow, DbError> {
    conn.query_row(
        "SELECT root_id, path, profile, created_at FROM storage_roots WHERE root_id = ?1",
        params![root_id],
        StorageRootRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(root_id.to_owned()),
        other => DbError::Sqlite(other),
    })
}

pub fn list_storage_roots(conn: &Connection) -> Result<Vec<StorageRootRow>, DbError> {
    let mut stmt =
        conn.prepare("SELECT root_id, path, profile, created_at FROM storage_roots ORDER BY path")?;
    let rows = stmt
        .query_map([], StorageRootRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn upsert_mount(conn: &Connection, row: &MountRow) -> Result<(), DbError> {
    conn.execute(
        "INSERT INTO mounts (
            mount_id, path, fs_type, device, queue_depth, read_concurrency,
            write_concurrency, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(mount_id) DO UPDATE SET
            path=excluded.path,
            fs_type=excluded.fs_type,
            device=excluded.device,
            queue_depth=excluded.queue_depth,
            read_concurrency=excluded.read_concurrency,
            write_concurrency=excluded.write_concurrency,
            updated_at=excluded.updated_at",
        params![
            row.mount_id,
            row.path,
            row.fs_type,
            row.device,
            row.queue_depth,
            row.read_concurrency,
            row.write_concurrency,
            row.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_mount(conn: &Connection, mount_id: &str) -> Result<MountRow, DbError> {
    conn.query_row(
        "SELECT mount_id, path, fs_type, device, queue_depth, read_concurrency,
                write_concurrency, updated_at
         FROM mounts WHERE mount_id = ?1",
        params![mount_id],
        MountRow::from_row,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(mount_id.to_owned()),
        other => DbError::Sqlite(other),
    })
}

pub fn list_mounts(conn: &Connection) -> Result<Vec<MountRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT mount_id, path, fs_type, device, queue_depth, read_concurrency,
                write_concurrency, updated_at
         FROM mounts ORDER BY path",
    )?;
    let rows = stmt
        .query_map([], MountRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;

    #[test]
    fn storage_root_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let row = StorageRootRow {
            root_id: "root-default".to_owned(),
            path: "/data".to_owned(),
            profile: "auto".to_owned(),
            created_at: 10,
        };
        upsert_storage_root(&conn, &row).unwrap();
        let fetched = get_storage_root(&conn, "root-default").unwrap();
        assert_eq!(fetched, row);
        assert_eq!(list_storage_roots(&conn).unwrap().len(), 1);
    }

    #[test]
    fn mount_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let row = MountRow {
            mount_id: "mount-dev-1".to_owned(),
            path: "/data".to_owned(),
            fs_type: Some("unknown".to_owned()),
            device: Some("dev:1".to_owned()),
            queue_depth: 4,
            read_concurrency: 2,
            write_concurrency: 1,
            updated_at: 20,
        };
        upsert_mount(&conn, &row).unwrap();
        let fetched = get_mount(&conn, "mount-dev-1").unwrap();
        assert_eq!(fetched, row);
        assert_eq!(list_mounts(&conn).unwrap().len(), 1);
    }
}
