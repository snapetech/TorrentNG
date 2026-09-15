//! Durable engine-wide peer ban persistence.

use std::{io, net::SocketAddr};

use rusqlite::{
    params,
    types::{Type, ValueRef},
    Connection, Row, Transaction,
};

use crate::error::DbError;

pub const MAX_PEER_BAN_ITEMS: usize = 65_536;
pub const MAX_PEER_BAN_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PEER_BAN_VALUE_BYTES: usize = 256;

fn peer_column_value_error(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Text,
        Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into())),
    )
}

fn bounded_peer_column(row: &Row<'_>) -> rusqlite::Result<String> {
    let value = match row.get_ref(0)? {
        ValueRef::Text(value) | ValueRef::Blob(value) => value,
        ValueRef::Null => return Err(peer_column_value_error("peer ban is NULL")),
        other => {
            return Err(peer_column_value_error(format!(
                "peer ban has unexpected SQLite type {other:?}"
            )))
        }
    };
    if value.len() > MAX_PEER_BAN_VALUE_BYTES {
        return Err(peer_column_value_error(format!(
            "peer ban is {} bytes; maximum is {MAX_PEER_BAN_VALUE_BYTES}",
            value.len()
        )));
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|error| peer_column_value_error(format!("peer ban is not UTF-8: {error}")))
}

/// Return the canonical textual peer addresses stored by the engine.
pub fn list_peer_bans(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut statement = conn.prepare(
        "SELECT peer,
                COUNT(*) OVER () AS result_count,
                COALESCE(SUM(length(CAST(peer AS BLOB))) OVER (), 0)
                    AS result_bytes
         FROM peer_bans
         ORDER BY peer ASC",
    )?;
    let mut rows = statement.query([])?;
    let Some(first) = rows.next()? else {
        return Ok(Vec::new());
    };
    let result_count = first.get::<_, i64>(1)?.max(0) as u64;
    let result_bytes = first.get::<_, i64>(2)?.max(0) as u64;
    if result_count > MAX_PEER_BAN_ITEMS as u64 {
        return Err(DbError::ValueTooLarge {
            field: "peer ban result items",
            len: result_count,
            max: MAX_PEER_BAN_ITEMS as u64,
        });
    }
    if result_bytes > MAX_PEER_BAN_RESULT_BYTES as u64 {
        return Err(DbError::ValueTooLarge {
            field: "peer ban result bytes",
            len: result_bytes,
            max: MAX_PEER_BAN_RESULT_BYTES as u64,
        });
    }
    let mut peers = Vec::with_capacity(result_count as usize);
    peers.push(bounded_peer_column(first)?);
    while let Some(row) = rows.next()? {
        peers.push(bounded_peer_column(row)?);
    }
    Ok(peers)
}

/// Insert a batch of peer addresses atomically with the caller's other
/// engine policy changes.
pub fn insert_peer_bans_in_tx(
    tx: &Transaction<'_>,
    peers: &[SocketAddr],
    created_at: i64,
) -> Result<(), DbError> {
    if peers.len() > MAX_PEER_BAN_ITEMS {
        return Err(DbError::ValueTooLarge {
            field: "peer ban input items",
            len: peers.len() as u64,
            max: MAX_PEER_BAN_ITEMS as u64,
        });
    }
    for peer in peers {
        tx.execute(
            "INSERT OR IGNORE INTO peer_bans (peer, created_at) VALUES (?1, ?2)",
            params![peer.to_string(), created_at],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::migrate;

    #[test]
    fn peer_bans_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let peer: SocketAddr = "192.0.2.10:6881".parse().unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        insert_peer_bans_in_tx(&tx, &[peer, peer], 10).unwrap();
        tx.commit().unwrap();
        assert_eq!(list_peer_bans(&conn).unwrap(), vec![peer.to_string()]);
    }

    #[test]
    fn peer_ban_reads_reject_oversized_values_before_materializing_them() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO peer_bans (peer, created_at) VALUES (?1, ?2)",
            params!["x".repeat(MAX_PEER_BAN_VALUE_BYTES + 1), 10],
        )
        .unwrap();

        assert!(list_peer_bans(&conn).is_err());
    }
}
