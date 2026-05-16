use rt_db::TorrentRow;

pub const SCALE_DATASET_SIZES: &[usize] = &[1_000, 5_000, 10_000, 15_000, 50_000];

#[derive(Debug, Clone)]
pub struct SyntheticTorrentDataset {
    pub torrents: Vec<TorrentRow>,
}

impl SyntheticTorrentDataset {
    pub fn new(count: usize) -> Self {
        let torrents = (0..count).map(synthetic_torrent_row).collect();
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
}

pub fn synthetic_torrent_row(index: usize) -> TorrentRow {
    let total_length = 64 * 1024 * 1024 + ((index % 8192) as i64 * 4096);
    let downloaded = if index % 5 == 0 {
        total_length / 2
    } else {
        total_length
    };
    let state = if downloaded == total_length {
        "seeding"
    } else {
        "downloading"
    };
    TorrentRow {
        info_hash: format!("{index:040x}"),
        name: format!("Synthetic Scale Torrent {index:08}"),
        total_length,
        piece_length: 256 * 1024,
        piece_count: (total_length + (256 * 1024 - 1)) / (256 * 1024),
        is_private: index % 3 != 0,
        save_path: format!("/certification/library/{:02}", index % 64),
        category: Some(format!("category-{:02}", index % 32)),
        tags: vec![format!("tag-{:02}", index % 128)],
        state: state.to_owned(),
        added_at: 1_700_000_000 + index as i64,
        completed_at: (downloaded == total_length).then_some(1_700_100_000 + index as i64),
        uploaded: downloaded.saturating_mul((index % 4) as i64),
        downloaded,
        ratio: if downloaded == 0 {
            0.0
        } else {
            downloaded.saturating_mul((index % 4) as i64) as f64 / downloaded as f64
        },
        trackers: vec![format!(
            "https://tracker-{:02}.example/announce/{}",
            index % 16,
            index
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_matrix_includes_certification_sizes() {
        let matrix = SyntheticTorrentDataset::scale_matrix();

        assert_eq!(matrix.len(), SCALE_DATASET_SIZES.len());
        assert_eq!(matrix[0].len(), 1_000);
        assert_eq!(matrix[3].len(), 15_000);
        assert_eq!(matrix[4].len(), 50_000);
    }

    #[test]
    fn synthetic_rows_are_stable_and_complete() {
        let row = synthetic_torrent_row(42);

        assert_eq!(row.info_hash.len(), 40);
        assert!(row.total_length > 0);
        assert!(row.piece_count > 0);
        assert!(row.trackers[0].contains("announce"));
        assert!(matches!(row.state.as_str(), "seeding" | "downloading"));
    }
}
