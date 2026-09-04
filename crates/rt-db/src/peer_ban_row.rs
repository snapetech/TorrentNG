//! Durable engine-wide peer ban persistence.

use std::net::SocketAddr;

use rusqlite::{params, Connection, Transaction};

use crate::error::DbError;

/// Return the canonical textual peer addresses stored by the engine.
pub fn list_peer_bans(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut statement = conn.prepare("SELECT peer FROM peer_bans ORDER BY peer ASC")?;
    let peers = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(peers)
}

/// Insert a batch of peer addresses atomically with the caller's other
/// engine policy changes.
pub fn insert_peer_bans_in_tx(
    tx: &Transaction<'_>,
    peers: &[SocketAddr],
    created_at: i64,
) -> Result<(), DbError> {
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
}
