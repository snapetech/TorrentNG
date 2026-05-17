/// Scale certification tests.
///
/// These tests verify that the sidecar API meets throughput targets from
/// docs/ENGINE.md at synthetic load levels.
///
/// Targets (from CLAUDE.md benchmarks section):
///   - 1k torrents:  GET /api/v1/torrents < 100ms
///   - 10k torrents: GET /api/v1/torrents < 200ms
///   - 15k torrents: GET /api/v1/torrents < 500ms
///   - 50k torrents: GET /api/qb/v2/torrents/info < 500ms
///   - native filter/sort over 15k torrents < 250ms
///   - sync/maindata delta < 50ms (at normal churn)
use std::time::{Duration, Instant};

use axum::{body::Body, http::Request};
use rt_api_native::{build_router, AppState};
use rt_api_qbit::AppState as QbState;
use rt_fastresume::{FastresumeState, ImportPolicy, PieceState};
use rt_path::{SafeRelPath, StorageProfile, StorageRootId};
use rt_piece_map::{FileSpan, PieceMap};
use rt_session::TorrentEntry;
use rt_storage::{
    IoClass, MountScheduler, PreallocationMode, SchedulerConfig, StorageIoConfig, VerifyResult,
};
use rt_tracker::backoff::jitter_interval;
use tower::ServiceExt;

/// Build a native API app populated with `n` synthetic torrents.
async fn native_app_with(n: usize) -> axum::Router {
    let state = AppState::new();
    {
        let mut reg = state.registry.write().await;
        for i in 0..n {
            let hash = format!("{:040x}", i);
            let entry = TorrentEntry::new(hash, format!("torrent_{i}"), "/data".into());
            reg.add(entry).unwrap();
        }
    }
    build_router(state)
}

/// Build a qBit API app populated with `n` synthetic torrents.
async fn qbit_app_with(n: usize) -> axum::Router {
    let state = QbState::new();
    {
        let mut reg = state.registry.write().await;
        for i in 0..n {
            let hash = format!("{:040x}", i);
            let entry = TorrentEntry::new(hash, format!("torrent_{i}"), "/data".into());
            reg.add(entry).unwrap();
        }
    }
    rt_api_qbit::build_qbit_router(state)
}

/// Debug builds are ~15x slower; multiply thresholds so CI passes without --release.
fn threshold(release_ms: u128) -> u128 {
    if cfg!(debug_assertions) {
        release_ms * 20
    } else {
        release_ms
    }
}

async fn get_ms(app: axum::Router, uri: &str) -> u128 {
    let t0 = Instant::now();
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    // Drain body so timing includes serialization.
    let _ = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    t0.elapsed().as_millis()
}

#[tokio::test]
async fn list_1k_torrents_under_100ms() {
    let app = native_app_with(1_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(100);
    assert!(ms < limit, "1k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn list_10k_torrents_under_200ms() {
    let app = native_app_with(10_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(200);
    assert!(ms < limit, "10k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn list_15k_torrents_under_500ms() {
    let app = native_app_with(15_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(500);
    assert!(ms < limit, "15k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn cold_db_load_15k_under_120s() {
    let dataset = rt_testkit::SyntheticTorrentDataset::new(15_000);
    let conn = rt_testkit::memory_db().unwrap();

    let t0 = Instant::now();
    dataset.write_to_db(&conn).unwrap();
    let rows = rt_db::list_all(&conn).unwrap();
    let ms = t0.elapsed().as_millis();

    assert_eq!(rows.len(), 15_000);
    let limit = threshold(120_000);
    assert!(ms < limit, "15k cold DB load took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn native_filter_sort_15k_under_250ms() {
    let app = native_app_with(15_000).await;
    let ms = get_ms(
        app,
        "/api/v1/torrents?filter=all&sort=name&reverse=true&limit=200&offset=2000",
    )
    .await;
    let limit = threshold(250);
    assert!(
        ms < limit,
        "15k native filter/sort took {ms}ms, want <{limit}ms"
    );
}

#[tokio::test]
async fn qbit_info_50k_under_500ms() {
    let app = qbit_app_with(50_000).await;
    let ms = get_ms(app, "/api/qb/v2/torrents/info").await;
    let limit = threshold(500);
    assert!(ms < limit, "50k qbit/info took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn sync_maindata_15k_under_50ms() {
    let app = qbit_app_with(15_000).await;
    let ms = get_ms(app, "/api/qb/v2/sync/maindata").await;
    let limit = threshold(50);
    assert!(ms < limit, "sync/maindata 15k took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn idle_memory_15k_under_2_5gb() {
    let before = current_rss_bytes();
    let app = qbit_app_with(15_000).await;
    let _ = get_ms(app, "/api/qb/v2/torrents/info?limit=200").await;
    let after = current_rss_bytes();
    let rss = after.unwrap_or(before.unwrap_or(0));
    let limit = 2_500_u64 * 1024 * 1024;

    assert!(rss < limit, "15k idle RSS is {rss} bytes, want <{limit}");
}

#[test]
fn tracker_restart_storm_15k_is_spread_by_jitter() {
    let interval = Duration::from_secs(30 * 60);
    let mut buckets = std::collections::BTreeMap::<u64, usize>::new();
    let mut min = u64::MAX;
    let mut max = 0;
    for _ in 0..15_000 {
        let seconds = jitter_interval(interval, 0.2).as_secs();
        min = min.min(seconds);
        max = max.max(seconds);
        *buckets.entry(seconds / 10).or_default() += 1;
    }

    let busiest_ten_second_bucket = buckets.values().copied().max().unwrap_or_default();
    assert!(min >= 1_440, "minimum jittered interval {min}s is too low");
    assert!(max <= 2_160, "maximum jittered interval {max}s is too high");
    assert!(
        max - min > 300,
        "jitter spread {}s is too narrow",
        max - min
    );
    assert!(
        busiest_ten_second_bucket < 750,
        "restart storm bucket too dense: {busiest_ten_second_bucket} announces in 10s"
    );
}

#[tokio::test]
async fn recheck_does_not_starve_seeding_peer_reads() {
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Hdd,
            max_queue: 256,
            recheck_concurrency: 1,
            peer_read_concurrency: 4,
            ..Default::default()
        },
    );
    let _recheck = scheduler.acquire(IoClass::Recheck).await.unwrap();

    let mut peer_read_permits = Vec::new();
    for _ in 0..4 {
        peer_read_permits.push(
            scheduler
                .try_acquire(IoClass::PeerRead)
                .expect("peer reads must not wait behind recheck permits"),
        );
    }

    assert_eq!(scheduler.available_permits(IoClass::Recheck), 0);
    assert_eq!(scheduler.available_permits(IoClass::PeerRead), 0);
}

#[tokio::test]
async fn storage_file_pool_stays_bounded_under_active_file_churn() {
    let dir = tempfile::tempdir().unwrap();
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Hdd,
            storage_io: StorageIoConfig {
                file_pool_size: 8,
                io_worker_threads: 4,
                io_queue_depth: 64,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    for i in 0..64 {
        let path = dir.path().join(format!("payload-{i}.bin"));
        std::fs::write(&path, vec![i as u8; 4096]).unwrap();
        let data = scheduler
            .read_at(IoClass::PeerRead, &path, 0, 1024)
            .await
            .unwrap();
        assert_eq!(data.len(), 1024);
    }

    let stats = scheduler.stats();
    assert_eq!(stats.file_pool.capacity, 8);
    assert!(
        stats.file_pool.open_files <= 8,
        "file pool leaked descriptors: {:?}",
        stats.file_pool
    );
    assert!(
        stats.file_pool.evictions >= 56,
        "expected LRU churn under capacity pressure: {:?}",
        stats.file_pool
    );
    assert_eq!(
        stats.read_ops_by_class[IoClass::PeerRead as usize],
        64,
        "every peer read should be accounted by class"
    );
}

#[tokio::test]
async fn storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("movie-piece.bin");
    let payload: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &payload).unwrap();
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Hdd,
            storage_io: StorageIoConfig {
                peer_read_readahead_bytes: 512 * 1024,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    for block in 0..32 {
        let offset = block * 16 * 1024;
        let data = scheduler
            .read_at(IoClass::PeerRead, &path, offset as u64, 16 * 1024)
            .await
            .unwrap();
        assert_eq!(&data[..], &payload[offset..offset + 16 * 1024]);
    }

    let stats = scheduler.stats();
    assert_eq!(stats.peer_read_cache_misses, 1);
    assert_eq!(stats.peer_read_cache_hits, 31);
    assert_eq!(stats.peer_read_cache_entries, 1);
    assert_eq!(
        stats.read_ops_by_class[IoClass::PeerRead as usize],
        32,
        "logical peer reads should still be accounted individually"
    );
    assert_eq!(
        stats.backend_read_ops_by_class[IoClass::PeerRead as usize],
        1,
        "adjacent peer reads should be served from one backend disk read"
    );
    assert_eq!(
        stats.backend_bytes_read_by_class[IoClass::PeerRead as usize],
        512 * 1024
    );
}

#[tokio::test]
async fn storage_positioned_io_preserves_offsets_under_concurrency() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("positioned-payload.bin");
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Ssd,
            storage_io: StorageIoConfig {
                io_worker_threads: 8,
                io_queue_depth: 128,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    scheduler
        .prepare_file(&path, 128 * 4096, PreallocationMode::Sparse)
        .await
        .unwrap();

    let mut writes = Vec::new();
    for block in 0..128 {
        let scheduler = scheduler.clone();
        let path = path.clone();
        writes.push(tokio::spawn(async move {
            let fill = (block % 251) as u8;
            scheduler
                .write_at(
                    IoClass::PeerWrite,
                    &path,
                    block * 4096,
                    bytes::Bytes::from(vec![fill; 4096]),
                    false,
                )
                .await
                .unwrap();
        }));
    }
    for write in writes {
        write.await.unwrap();
    }

    for block in [0_u64, 1, 31, 64, 127] {
        let data = scheduler
            .read_at(IoClass::Foreground, &path, block * 4096, 4096)
            .await
            .unwrap();
        assert!(data.iter().all(|byte| *byte == (block % 251) as u8));
    }

    let stats = scheduler.stats();
    assert_eq!(stats.write_ops_by_class[IoClass::PeerWrite as usize], 128);
    assert_eq!(
        stats.bytes_written_by_class[IoClass::PeerWrite as usize],
        128 * 4096
    );
}

#[tokio::test]
async fn storage_hash_pool_does_not_block_peer_read_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seed.bin");
    std::fs::write(&path, vec![7_u8; 64 * 1024]).unwrap();
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Hdd,
            storage_io: StorageIoConfig {
                hash_worker_threads: 1,
                hash_queue_depth: 2,
                io_worker_threads: 2,
                io_queue_depth: 16,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let mut hash_tasks = Vec::new();
    for i in 0..8 {
        let scheduler = scheduler.clone();
        hash_tasks.push(tokio::spawn(async move {
            let data = bytes::Bytes::from(vec![i as u8; 1024 * 1024]);
            scheduler.hash_sha1(data).await.unwrap()
        }));
    }

    let read = tokio::time::timeout(
        Duration::from_millis(threshold(50) as u64),
        scheduler.read_at(IoClass::PeerRead, &path, 0, 16 * 1024),
    )
    .await
    .expect("peer read should not wait behind hash queue")
    .unwrap();
    assert_eq!(read.len(), 16 * 1024);

    for task in hash_tasks {
        let hash = task.await.unwrap();
        assert_ne!(hash, [0; 20]);
    }

    let stats = scheduler.stats();
    assert_eq!(stats.hash_ops, 8);
    assert_eq!(stats.read_ops_by_class[IoClass::PeerRead as usize], 1);
}

#[tokio::test]
async fn storage_recheck_hashing_reports_scheduler_result_without_runtime_stall() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recheck.bin");
    let content = b"piece-zero-piece-one";
    std::fs::write(&path, content).unwrap();
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Hdd,
            recheck_concurrency: 1,
            peer_read_concurrency: 4,
            ..Default::default()
        },
    );
    let hash = scheduler
        .hash_sha1(bytes::Bytes::copy_from_slice(content))
        .await
        .unwrap();
    let piece_map = PieceMap::new(
        content.len() as u64,
        vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("recheck.bin", false).unwrap(),
            content_offset: 0,
            length: content.len() as u64,
        }],
    )
    .unwrap();
    let hashes = [hash];
    let verifier = rt_storage::PieceVerifier::new(dir.path(), &scheduler, &piece_map, &hashes);

    let result = tokio::time::timeout(Duration::from_secs(2), verifier.verify_piece(0))
        .await
        .expect("recheck hashing should finish without stalling the runtime");
    assert_eq!(result, VerifyResult::Valid);

    let stats = scheduler.stats();
    assert_eq!(stats.read_ops_by_class[IoClass::Recheck as usize], 1);
    assert!(stats.hash_ops >= 2);
}

#[tokio::test]
async fn completed_piece_ram_hash_avoids_read_after_write_backend_reads() {
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Ssd,
            ..Default::default()
        },
    );
    let piece = bytes::Bytes::from(vec![0x42u8; 1024 * 1024]);
    let hash = scheduler.hash_sha1(piece.clone()).await.unwrap();
    let before = scheduler.stats();
    let verified = scheduler.hash_sha1(piece).await.unwrap();
    let after = scheduler.stats();

    assert_eq!(verified, hash);
    let before_backend_reads = before.backend_read_ops_by_class.iter().sum::<u64>();
    let after_backend_reads = after.backend_read_ops_by_class.iter().sum::<u64>();
    assert_eq!(
        after_backend_reads - before_backend_reads,
        0,
        "RAM piece verification must not perform read-after-write disk I/O"
    );
    assert_eq!(after.hash_ops - before.hash_ops, 1);
}

#[test]
fn crash_watermark_bounds_restart_recheck_to_dirty_pieces() {
    let piece_count = 100_000u32;
    let dirty: Vec<u32> = (0..512).map(|piece| piece * 3).collect();
    let mut state =
        FastresumeState::new_empty(&[9u8; 20], piece_count, ImportPolicy::RequireVerification);
    state.pieces.fill(PieceState::Valid);
    state.clean_shutdown = false;
    state.set_dirty_pieces_since_barrier(dirty.iter().copied());

    let downgraded = state
        .apply_unclean_shutdown_watermark()
        .expect("watermark should bound unclean restart recheck");

    assert_eq!(downgraded, dirty.len() as u32);
    assert_eq!(state.unknown_piece_count(), dirty.len() as u32);
    assert_eq!(
        state.valid_piece_count(),
        piece_count - dirty.len() as u32,
        "restart should not force a full-library recheck"
    );
    assert!(state.clean_shutdown);
    assert!(state.durability.dirty_pieces_since_barrier.is_empty());
}

#[tokio::test]
async fn sparse_recheck_skips_holes_and_reports_extent_counters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse-scale.bin");
    let file_len = 4 * 1024 * 1024usize;
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(file_len as u64).unwrap();
    drop(file);
    let data = vec![0x33u8; 16 * 1024];
    let data_offset = 2 * 1024 * 1024usize;
    std::fs::write(dir.path().join("payload.tmp"), &data).unwrap();
    let scheduler = MountScheduler::new(
        StorageRootId::new(),
        &SchedulerConfig {
            profile: StorageProfile::Ssd,
            ..Default::default()
        },
    );
    scheduler
        .write_at(
            IoClass::PeerWrite,
            &path,
            data_offset as u64,
            bytes::Bytes::from(data.clone()),
            false,
        )
        .await
        .unwrap();

    let mut expected = vec![0u8; file_len];
    expected[data_offset..data_offset + data.len()].copy_from_slice(&data);
    let expected_hash = scheduler
        .hash_sha1(bytes::Bytes::from(expected))
        .await
        .unwrap();
    let piece_map = PieceMap::new(
        file_len as u64,
        vec![FileSpan {
            file_index: 0,
            path: SafeRelPath::from_name("sparse-scale.bin", false).unwrap(),
            content_offset: 0,
            length: file_len as u64,
        }],
    )
    .unwrap();
    let hashes = [expected_hash];
    let verifier = rt_storage::PieceVerifier::new(dir.path(), &scheduler, &piece_map, &hashes);

    assert_eq!(verifier.verify_piece(0).await, VerifyResult::Valid);
    let stats = scheduler.stats();
    assert!(
        stats.sparse_data_extents > 0 || stats.sparse_seek_fallbacks > 0,
        "sparse recheck should either map extents or record an unsupported-filesystem fallback"
    );
}

fn current_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kb = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())?;
    Some(kb * 1024)
}
