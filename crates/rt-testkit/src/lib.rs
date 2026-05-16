//! Shared test fixtures for rtorrentNG crates.
//!
//! This crate intentionally stays small and dependency-light. It provides
//! deterministic database and torrent-row fixtures that integration tests can
//! reuse without copying setup boilerplate between crates.

use rusqlite::Connection;

/// Deterministic timestamp used by generated fixtures.
pub const FIXTURE_ADDED_AT: i64 = 1_700_000_000;

/// Native engine scale certification dataset sizes.
pub const SCALE_DATASET_SIZES: &[usize] = &[1_000, 5_000, 10_000, 15_000, 50_000];

#[derive(Debug, Clone)]
pub struct SyntheticTorrentDataset {
    pub torrents: Vec<rt_db::TorrentRow>,
}

impl SyntheticTorrentDataset {
    pub fn new(count: usize) -> Self {
        let torrents = (0..count).map(torrent_row).collect();
        Self { torrents }
    }

    pub fn scale_matrix() -> Vec<Self> {
        SCALE_DATASET_SIZES.iter().copied().map(Self::new).collect()
    }

    pub fn len(&self) -> usize {
        self.torrents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.torrents.is_empty()
    }

    pub fn total_bytes(&self) -> i64 {
        self.torrents
            .iter()
            .map(|torrent| torrent.total_length)
            .sum()
    }

    pub fn write_to_db(&self, conn: &Connection) -> Result<(), rt_db::DbError> {
        rt_db::migrate(conn)?;
        for torrent in &self.torrents {
            rt_db::upsert(conn, torrent)?;
        }
        Ok(())
    }
}

/// Open an in-memory SQLite database and apply the rtorrentNG schema.
pub fn memory_db() -> Result<Connection, rt_db::DbError> {
    let conn = Connection::open_in_memory()?;
    rt_db::migrate(&conn)?;
    Ok(conn)
}

/// Build a deterministic 40-character hex SHA-1-like info hash.
pub fn fixture_info_hash(index: usize) -> String {
    format!("{index:040x}")
}

/// Build a deterministic torrent row suitable for database and API tests.
pub fn torrent_row(index: usize) -> rt_db::TorrentRow {
    rt_db::TorrentRow {
        info_hash: fixture_info_hash(index),
        name: format!("fixture-{index:04}"),
        total_length: 1024 * 1024 * (index as i64 + 1),
        piece_length: 16 * 1024,
        piece_count: 64 * (index as i64 + 1),
        is_private: index % 2 == 0,
        save_path: format!("/data/fixture-{index:04}"),
        category: Some(if index % 2 == 0 { "movies" } else { "tv" }.to_owned()),
        tags: vec!["fixture".to_owned(), format!("batch-{}", index % 10)],
        state: if index % 3 == 0 {
            "seeding".to_owned()
        } else {
            "downloading".to_owned()
        },
        added_at: FIXTURE_ADDED_AT + index as i64,
        completed_at: (index % 3 == 0).then_some(FIXTURE_ADDED_AT + index as i64 + 60),
        uploaded: 10_000 * index as i64,
        downloaded: 20_000 * index as i64,
        ratio: if index == 0 { 0.0 } else { 0.5 },
        trackers: vec![format!("https://tracker.example/{index:04}/announce")],
    }
}

/// Insert `count` deterministic torrents into a migrated database.
pub fn seed_torrents(
    conn: &Connection,
    count: usize,
) -> Result<Vec<rt_db::TorrentRow>, rt_db::DbError> {
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let row = torrent_row(index);
        rt_db::upsert(conn, &row)?;
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_db_is_migrated() {
        let conn = memory_db().unwrap();

        let rows = rt_db::list_all(&conn).unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn seed_torrents_inserts_deterministic_rows() {
        let conn = memory_db().unwrap();

        let seeded = seed_torrents(&conn, 3).unwrap();
        let rows = rt_db::list_all(&conn).unwrap();

        assert_eq!(seeded.len(), 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rt_db::get(&conn, &fixture_info_hash(1)).unwrap().name,
            "fixture-0001"
        );
    }

    #[test]
    fn scale_matrix_includes_certification_sizes() {
        let matrix = SyntheticTorrentDataset::scale_matrix();

        assert_eq!(matrix.len(), SCALE_DATASET_SIZES.len());
        assert_eq!(matrix[0].len(), 1_000);
        assert_eq!(matrix[3].len(), 15_000);
        assert_eq!(matrix[4].len(), 50_000);
    }

    #[test]
    fn synthetic_dataset_writes_to_db() {
        let conn = memory_db().unwrap();
        let dataset = SyntheticTorrentDataset::new(100);

        dataset.write_to_db(&conn).unwrap();

        assert_eq!(rt_db::list_all(&conn).unwrap().len(), 100);
        assert!(dataset.total_bytes() > 0);
    }
}
