use rusqlite::{params, Connection};

use crate::error::DbError;

/// Settings are the durable control-plane store for compatibility state and
/// small engine values. Keep a corrupt or stale database row from becoming a
/// multi-megabyte allocation every time the daemon restores a facade.
pub const MAX_SETTING_KEY_BYTES: usize = 256;
pub const MAX_SETTING_VALUE_BYTES: usize = 1024 * 1024;

fn validate_setting(key: &str, value: Option<&str>) -> Result<(), DbError> {
    if key.len() > MAX_SETTING_KEY_BYTES {
        return Err(DbError::ValueTooLarge {
            field: "setting key",
            len: key.len() as u64,
            max: MAX_SETTING_KEY_BYTES as u64,
        });
    }
    if let Some(value) = value {
        if value.len() > MAX_SETTING_VALUE_BYTES {
            return Err(DbError::ValueTooLarge {
                field: "setting value",
                len: value.len() as u64,
                max: MAX_SETTING_VALUE_BYTES as u64,
            });
        }
    }
    Ok(())
}

pub fn set_setting(
    conn: &Connection,
    key: &str,
    value: &str,
    updated_at: i64,
) -> Result<(), DbError> {
    validate_setting(key, Some(value))?;
    conn.execute(
        "INSERT INTO settings (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
            value=excluded.value,
            updated_at=excluded.updated_at",
        params![key, value, updated_at],
    )?;
    Ok(())
}

/// Persist a setting as part of a caller-owned transaction.
///
/// Settings are also used for small durable control-plane collections (for
/// example, qBittorrent-compatible global tag definitions).  Keeping the
/// transaction form here prevents those definitions from being committed
/// separately from the rows they affect.
pub fn set_setting_in_tx(
    tx: &rusqlite::Transaction<'_>,
    key: &str,
    value: &str,
    updated_at: i64,
) -> Result<(), DbError> {
    validate_setting(key, Some(value))?;
    tx.execute(
        "INSERT INTO settings (key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
            value=excluded.value,
            updated_at=excluded.updated_at",
        params![key, value, updated_at],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<String, DbError> {
    validate_setting(key, None)?;
    let value_bytes: i64 = conn
        .query_row(
            "SELECT length(CAST(value AS BLOB)) FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(key.to_owned()),
            other => DbError::Sqlite(other),
        })?;
    let value_bytes = value_bytes.max(0) as u64;
    if value_bytes > MAX_SETTING_VALUE_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "setting value",
            len: value_bytes,
            max: MAX_SETTING_VALUE_BYTES as u64,
        });
    }
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => DbError::NotFound(key.to_owned()),
        other => DbError::Sqlite(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;
    use rusqlite::Connection;

    #[test]
    fn setting_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        set_setting(&conn, "api.bind", "127.0.0.1:8080", 10).unwrap();
        assert_eq!(get_setting(&conn, "api.bind").unwrap(), "127.0.0.1:8080");
        set_setting(&conn, "api.bind", "0.0.0.0:8080", 20).unwrap();
        assert_eq!(get_setting(&conn, "api.bind").unwrap(), "0.0.0.0:8080");
    }

    #[test]
    fn setting_can_be_updated_inside_a_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        set_setting_in_tx(&tx, "session.tags", "[\"hd\"]", 10).unwrap();
        tx.commit().unwrap();
        assert_eq!(get_setting(&conn, "session.tags").unwrap(), "[\"hd\"]");
    }

    #[test]
    fn setting_values_are_bounded_on_write_and_read() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let oversized = "x".repeat(MAX_SETTING_VALUE_BYTES + 1);
        assert!(matches!(
            set_setting(&conn, "too-large", &oversized, 10),
            Err(DbError::ValueTooLarge {
                field: "setting value",
                ..
            })
        ));

        // A pre-existing/corrupt database can bypass the write helper. The
        // size probe must reject it before the value is converted to String.
        conn.execute(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params!["corrupt", oversized, 10],
        )
        .unwrap();
        assert!(matches!(
            get_setting(&conn, "corrupt"),
            Err(DbError::ValueTooLarge {
                field: "setting value",
                ..
            })
        ));
    }
}
