use std::path::Path;
use std::time::Instant;

use rt_bencode::{encode, BValue};
use rt_metainfo::{parse_torrent, TorrentMeta};
use rt_migrate::{dry_run_qbittorrent_backup, ImportOptions};

const IMPORT_CERT_TORRENTS: usize = 15_000;

fn write_torrent(path: &Path, index: usize) -> String {
    let pieces = vec![(index % 251) as u8; 20];
    let name = format!("import-cert-{index:05}.bin");
    let length = 16_384 + index as i64;
    let mut info = vec![
        (b"length".as_slice(), BValue::Int(length)),
        (b"name".as_slice(), BValue::Bytes(name.as_bytes())),
        (b"piece length".as_slice(), BValue::Int(16_384)),
        (b"pieces".as_slice(), BValue::Bytes(&pieces)),
    ];
    info.sort_by(|a, b| a.0.cmp(b.0));
    let announce = format!("https://tracker-{:02}.example/announce", index % 16);
    let mut torrent = vec![
        (b"announce".as_slice(), BValue::Bytes(announce.as_bytes())),
        (b"info".as_slice(), BValue::Dict(info)),
    ];
    torrent.sort_by(|a, b| a.0.cmp(b.0));
    let raw = encode(&BValue::Dict(torrent));
    std::fs::write(path, &raw).unwrap();
    match parse_torrent(&raw).unwrap() {
        TorrentMeta::V1(meta) | TorrentMeta::Hybrid(meta, _) => hex_lower(&meta.info_hash),
        TorrentMeta::V2(_) => unreachable!(),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn threshold(release_ms: u128) -> u128 {
    if cfg!(debug_assertions) {
        release_ms * 20
    } else {
        release_ms
    }
}

#[test]
fn qbit_15k_dry_run_import_is_certified() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..IMPORT_CERT_TORRENTS {
        let temp_torrent = dir.path().join(format!("fixture-{index:05}.torrent"));
        let info_hash = write_torrent(&temp_torrent, index);
        std::fs::rename(
            &temp_torrent,
            dir.path().join(format!("{info_hash}.torrent")),
        )
        .unwrap();
        let resume = serde_json::json!({
            "save_path": format!("/cert/import/{:02}", index % 64),
            "category": format!("category-{:02}", index % 32),
            "tags": [format!("tag-{:03}", index % 128)],
            "uploaded": index as u64 * 10,
            "downloaded": 16_384 + index as u64,
            "completed": true
        });
        std::fs::write(
            dir.path().join(format!("{info_hash}.fastresume")),
            serde_json::to_vec(&resume).unwrap(),
        )
        .unwrap();
    }

    let started = Instant::now();
    let plan = dry_run_qbittorrent_backup(dir.path()).unwrap();
    let elapsed_ms = started.elapsed().as_millis();

    assert_eq!(plan.torrent_count(), IMPORT_CERT_TORRENTS);
    assert_eq!(plan.skipped.len(), 0);
    assert_eq!(plan.warning_count(), 0);
    assert!(plan.to_markdown().contains("Importable torrents: 15000"));

    let import = plan.to_db_import(&ImportOptions {
        default_save_path: None,
        added_at: 1_700_000_000,
        ..ImportOptions::default()
    });
    assert_eq!(import.torrents.len(), IMPORT_CERT_TORRENTS);
    assert_eq!(
        import
            .torrents
            .iter()
            .map(|torrent| torrent.files.len())
            .sum::<usize>(),
        IMPORT_CERT_TORRENTS
    );
    assert_eq!(
        import
            .torrents
            .iter()
            .map(|torrent| torrent.trackers.len())
            .sum::<usize>(),
        IMPORT_CERT_TORRENTS
    );

    let limit = threshold(30_000);
    assert!(
        elapsed_ms < limit,
        "15k dry-run import took {elapsed_ms}ms, want <{limit}ms"
    );
}
