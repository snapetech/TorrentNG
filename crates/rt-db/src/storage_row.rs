use rusqlite::types::{Type, ValueRef};
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use std::io;

use crate::error::DbError;

pub const MAX_STORAGE_ROOT_RESULT_ITEMS: usize = 1_024;
pub const MAX_STORAGE_ROOT_RESULT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MOUNT_RESULT_ITEMS: usize = 1_024;
pub const MAX_MOUNT_RESULT_BYTES: usize = 16 * 1024 * 1024;

const MAX_STORAGE_ID_BYTES: usize = 256;
const MAX_STORAGE_PATH_BYTES: usize = 16 * 1024;
const MAX_STORAGE_PROFILE_BYTES: usize = 256;
const MAX_MOUNT_TEXT_BYTES: usize = 256;

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
            root_id: bounded_storage_text(row, 0, "storage root id", MAX_STORAGE_ID_BYTES)?,
            path: bounded_storage_text(row, 1, "storage root path", MAX_STORAGE_PATH_BYTES)?,
            profile: bounded_storage_text(
                row,
                2,
                "storage root profile",
                MAX_STORAGE_PROFILE_BYTES,
            )?,
            created_at: row.get(3)?,
        })
    }
}

impl MountRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(MountRow {
            mount_id: bounded_storage_text(row, 0, "mount id", MAX_STORAGE_ID_BYTES)?,
            path: bounded_storage_text(row, 1, "mount path", MAX_STORAGE_PATH_BYTES)?,
            fs_type: optional_bounded_storage_text(
                row,
                2,
                "mount filesystem type",
                MAX_MOUNT_TEXT_BYTES,
            )?,
            device: optional_bounded_storage_text(row, 3, "mount device", MAX_MOUNT_TEXT_BYTES)?,
            queue_depth: row.get(4)?,
            read_concurrency: row.get(5)?,
            write_concurrency: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }
}

fn storage_column_value_error(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn storage_text_value<'a>(
    row: &'a Row<'_>,
    column: usize,
    field: &str,
) -> rusqlite::Result<Option<&'a [u8]>> {
    match row.get_ref(column)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => Ok(Some(value)),
        ValueRef::Null => Ok(None),
        other => Err(storage_column_value_error(
            column,
            format!("{field} has unexpected SQLite type {other:?}"),
        )),
    }
}

fn bounded_storage_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<String> {
    let Some(value) = storage_text_value(row, column, field)? else {
        return Err(storage_column_value_error(
            column,
            format!("{field} is NULL"),
        ));
    };
    if value.len() > maximum {
        return Err(storage_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| {
            storage_column_value_error(column, format!("{field} is not UTF-8: {error}"))
        })
}

fn optional_bounded_storage_text(
    row: &Row<'_>,
    column: usize,
    field: &str,
    maximum: usize,
) -> rusqlite::Result<Option<String>> {
    let Some(value) = storage_text_value(row, column, field)? else {
        return Ok(None);
    };
    if value.len() > maximum {
        return Err(storage_column_value_error(
            column,
            format!("{field} is {} bytes; maximum is {maximum}", value.len()),
        ));
    }
    std::str::from_utf8(value)
        .map(|value| Some(value.to_owned()))
        .map_err(|error| {
            storage_column_value_error(column, format!("{field} is not UTF-8: {error}"))
        })
}

fn validate_storage_text(value: &str, field: &'static str, maximum: usize) -> Result<(), DbError> {
    if value.len() > maximum {
        return Err(DbError::ValueTooLarge {
            field,
            len: value.len() as u64,
            max: maximum as u64,
        });
    }
    Ok(())
}

fn validate_optional_storage_text(
    value: Option<&str>,
    field: &'static str,
    maximum: usize,
) -> Result<(), DbError> {
    if let Some(value) = value {
        validate_storage_text(value, field, maximum)?;
    }
    Ok(())
}

fn validate_storage_root(row: &StorageRootRow) -> Result<(), DbError> {
    validate_storage_text(&row.root_id, "storage root id", MAX_STORAGE_ID_BYTES)?;
    validate_storage_text(&row.path, "storage root path", MAX_STORAGE_PATH_BYTES)?;
    validate_storage_text(
        &row.profile,
        "storage root profile",
        MAX_STORAGE_PROFILE_BYTES,
    )
}

fn validate_mount(row: &MountRow) -> Result<(), DbError> {
    validate_storage_text(&row.mount_id, "mount id", MAX_STORAGE_ID_BYTES)?;
    validate_storage_text(&row.path, "mount path", MAX_STORAGE_PATH_BYTES)?;
    validate_optional_storage_text(
        row.fs_type.as_deref(),
        "mount filesystem type",
        MAX_MOUNT_TEXT_BYTES,
    )?;
    validate_optional_storage_text(row.device.as_deref(), "mount device", MAX_MOUNT_TEXT_BYTES)
}

pub fn upsert_storage_root(conn: &Connection, row: &StorageRootRow) -> Result<(), DbError> {
    validate_storage_root(row)?;
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
    let mut stmt = conn.prepare(
        "SELECT root_id, path, profile, created_at,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(root_id AS BLOB)), 0) +
                    COALESCE(length(CAST(path AS BLOB)), 0) +
                    COALESCE(length(CAST(profile AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM storage_roots
         ORDER BY path",
    )?;
    let mut rows = stmt.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(4)?;
    if result_count < 0 || result_count as u64 > MAX_STORAGE_ROOT_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "storage root result items",
            len: result_count.max(0) as u64,
            max: MAX_STORAGE_ROOT_RESULT_ITEMS as u64,
        });
    }
    let result_bytes = first.get::<_, i64>(5)?;
    if result_bytes < 0 || result_bytes as u64 > MAX_STORAGE_ROOT_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "storage root result bytes",
            len: result_bytes.max(0) as u64,
            max: MAX_STORAGE_ROOT_RESULT_BYTES as u64,
        });
    }
    let mut result = Vec::with_capacity(result_count as usize);
    result.push(StorageRootRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(StorageRootRow::from_row(row)?);
    }
    Ok(result)
}

pub fn upsert_mount(conn: &Connection, row: &MountRow) -> Result<(), DbError> {
    validate_mount(row)?;
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
                write_concurrency, updated_at,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(
                    COALESCE(length(CAST(mount_id AS BLOB)), 0) +
                    COALESCE(length(CAST(path AS BLOB)), 0) +
                    COALESCE(length(CAST(fs_type AS BLOB)), 0) +
                    COALESCE(length(CAST(device AS BLOB)), 0)
                ) OVER (), 0) AS result_bytes
         FROM mounts ORDER BY path",
    )?;
    let mut rows = stmt.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(8)?;
    if result_count < 0 || result_count as u64 > MAX_MOUNT_RESULT_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "mount result items",
            len: result_count.max(0) as u64,
            max: MAX_MOUNT_RESULT_ITEMS as u64,
        });
    }
    let result_bytes = first.get::<_, i64>(9)?;
    if result_bytes < 0 || result_bytes as u64 > MAX_MOUNT_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "mount result bytes",
            len: result_bytes.max(0) as u64,
            max: MAX_MOUNT_RESULT_BYTES as u64,
        });
    }
    let mut result = Vec::with_capacity(result_count as usize);
    result.push(MountRow::from_row(first)?);
    while let Some(row) = rows.next()? {
        result.push(MountRow::from_row(row)?);
    }
    Ok(result)
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

    #[test]
    fn storage_root_result_rejects_too_many_rows() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for index in 0..=MAX_STORAGE_ROOT_RESULT_ITEMS {
            conn.execute(
                "INSERT INTO storage_roots (root_id, path, profile, created_at)
                 VALUES (?1, ?2, 'auto', ?3)",
                params![
                    format!("root-{index}"),
                    format!("/data/{index}"),
                    index as i64
                ],
            )
            .unwrap();
        }

        assert!(matches!(
            list_storage_roots(&conn),
            Err(DbError::ValueTooLarge {
                field: "storage root result items",
                ..
            })
        ));
    }

    #[test]
    fn storage_root_result_rejects_too_many_bytes() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for index in 0..MAX_STORAGE_ROOT_RESULT_ITEMS {
            conn.execute(
                "INSERT INTO storage_roots (root_id, path, profile, created_at)
                 VALUES (?1, ?2, 'auto', ?3)",
                params![
                    format!("root-{index}"),
                    format!("/{}-{index}", "x".repeat(MAX_STORAGE_PATH_BYTES - 8)),
                    index as i64
                ],
            )
            .unwrap();
        }

        assert!(matches!(
            list_storage_roots(&conn),
            Err(DbError::ValueTooLarge {
                field: "storage root result bytes",
                ..
            })
        ));
    }

    #[test]
    fn storage_rows_reject_oversized_writes() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let root = StorageRootRow {
            root_id: "root".to_owned(),
            path: "x".repeat(MAX_STORAGE_PATH_BYTES + 1),
            profile: "auto".to_owned(),
            created_at: 1,
        };
        assert!(matches!(
            upsert_storage_root(&conn, &root),
            Err(DbError::ValueTooLarge {
                field: "storage root path",
                ..
            })
        ));
    }

    #[test]
    fn mount_result_rejects_too_many_rows() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for index in 0..=MAX_MOUNT_RESULT_ITEMS {
            conn.execute(
                "INSERT INTO mounts (
                    mount_id, path, fs_type, device, queue_depth, read_concurrency,
                    write_concurrency, updated_at
                 ) VALUES (?1, ?2, 'unknown', NULL, 1, 1, 1, ?3)",
                params![
                    format!("mount-{index}"),
                    format!("/data/{index}"),
                    index as i64
                ],
            )
            .unwrap();
        }

        assert!(matches!(
            list_mounts(&conn),
            Err(DbError::ValueTooLarge {
                field: "mount result items",
                ..
            })
        ));
    }
}
