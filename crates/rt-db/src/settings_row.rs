use rusqlite::{params, Connection};

use crate::error::DbError;

pub fn set_setting(
    conn: &Connection,
    key: &str,
    value: &str,
    updated_at: i64,
) -> Result<(), DbError> {
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

pub fn get_setting(conn: &Connection, key: &str) -> Result<String, DbError> {
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
}
