use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rt_path::{StorageProfile, StorageRootId};
use rt_storage::{
    detect_storage_topology, IoClass, MountScheduler, SchedulerConfig, StorageIoConfig,
};

fn bench_size(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn bench_u64(name: &str, default: u64) -> u64 {
    bench_size(name, default)
}

fn bench_dir() -> tempfile::TempDir {
    let root = std::env::var_os("TNG_STORAGE_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("tng-storage-bench-")
        .tempdir_in(root)
        .unwrap()
}

fn print_topology(path: &Path) {
    let topology = detect_storage_topology(path);
    println!(
        "tng_storage_bench_path={} profile={:?} fs={:?} cow={} device={:?}",
        path.display(),
        topology.profile,
        topology.fs_type,
        topology.cow,
        topology.device_id,
    );
}

fn shuffled_offsets(blocks: u64, block_len: usize) -> Vec<u64> {
    (0..blocks)
        .map(|i| i.wrapping_mul(1_103_515_245) % blocks * block_len as u64)
        .collect()
}

#[cfg(unix)]
fn drop_file_cache(path: &Path) {
    use std::os::fd::AsRawFd;

    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let _ = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
}

#[cfg(not(unix))]
fn drop_file_cache(_path: &Path) {}

fn settle_file_for_read_benchmark(path: &Path) {
    let file = std::fs::OpenOptions::new().read(true).open(path).unwrap();
    file.sync_all().unwrap();
    drop(file);
    drop_file_cache(path);
}

async fn run_reads(
    scheduler: MountScheduler,
    path: PathBuf,
    data: &[u8],
    offsets: &[u64],
    block_len: usize,
) -> Duration {
    let started = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for &offset in offsets {
        let scheduler = scheduler.clone();
        let path = path.clone();
        set.spawn(async move {
            let bytes = scheduler
                .read_at(IoClass::PeerRead, &path, offset, block_len)
                .await
                .unwrap();
            (offset, bytes)
        });
    }

    while let Some(joined) = set.join_next().await {
        let (offset, bytes) = joined.unwrap();
        let start = offset as usize;
        assert_eq!(&bytes[..], &data[start..start + block_len]);
    }
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-device storage benchmark; run explicitly with --ignored --nocapture"]
async fn peer_read_readahead_reduces_backend_reads_on_adjacent_blocks() {
    let blocks = bench_size("TNG_STORAGE_BENCH_BLOCKS", 4096);
    let block_len = bench_size("TNG_STORAGE_BENCH_BLOCK_LEN", 16 * 1024) as usize;
    let total = blocks as usize * block_len;

    let dir = bench_dir();
    print_topology(dir.path());
    let path = dir.path().join("adjacent.bin");
    let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();
    settle_file_for_read_benchmark(&path);

    let scheduler = MountScheduler::new_for_path(
        StorageRootId::new(),
        dir.path(),
        &SchedulerConfig {
            profile: StorageProfile::Unknown,
            peer_read_concurrency: blocks as usize,
            storage_io: StorageIoConfig {
                peer_read_readahead_bytes: 512 * 1024,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let offsets: Vec<u64> = (0..blocks).map(|i| i * block_len as u64).collect();
    let elapsed = run_reads(scheduler.clone(), path, &data, &offsets, block_len).await;
    let stats = scheduler.stats();
    let submitted = stats.read_ops_by_class[IoClass::PeerRead as usize];
    let backend_reads = stats.backend_read_ops_by_class[IoClass::PeerRead as usize];
    let reduction = submitted as f64 / backend_reads.max(1) as f64;

    println!(
        "tng_storage_readahead submitted={submitted} backend_reads={backend_reads} reduction={reduction:.2}x elapsed_ms={}",
        elapsed.as_millis(),
    );

    assert_eq!(submitted, blocks);
    assert!(
        backend_reads * 5 <= submitted,
        "expected >=5x backend read reduction; submitted={submitted} backend_reads={backend_reads}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-device storage benchmark; run explicitly with --ignored --nocapture"]
async fn repeated_reads_reuse_one_open_file_handle() {
    let reads = bench_size("TNG_STORAGE_BENCH_READS", 10_000);
    let block_len = bench_size("TNG_STORAGE_BENCH_BLOCK_LEN", 16 * 1024) as usize;
    let total = block_len * 2;

    let dir = bench_dir();
    print_topology(dir.path());
    let path = dir.path().join("hot.bin");
    let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();
    settle_file_for_read_benchmark(&path);

    let scheduler = MountScheduler::new_for_path(
        StorageRootId::new(),
        dir.path(),
        &SchedulerConfig {
            profile: StorageProfile::Unknown,
            storage_io: StorageIoConfig {
                file_pool_size: 64,
                peer_read_readahead_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    for i in 0..reads {
        let offset = (i % 2) * block_len as u64;
        let bytes = scheduler
            .read_at(IoClass::Foreground, &path, offset, block_len)
            .await
            .unwrap();
        let start = offset as usize;
        assert_eq!(&bytes[..], &data[start..start + block_len]);
    }

    let stats = scheduler.file_pool_stats();
    println!(
        "tng_storage_file_pool reads={reads} hits={} misses={} open_files={} capacity={}",
        stats.hits, stats.misses, stats.open_files, stats.capacity
    );

    assert_eq!(stats.misses, 1);
    assert_eq!(stats.open_files, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-device storage benchmark; run explicitly with --ignored --nocapture"]
async fn shuffled_peer_read_baseline_reports_current_scheduler_throughput() {
    let blocks = bench_size("TNG_STORAGE_BENCH_BLOCKS", 4096);
    let block_len = bench_size("TNG_STORAGE_BENCH_BLOCK_LEN", 16 * 1024) as usize;
    let total = blocks as usize * block_len;

    let dir = bench_dir();
    print_topology(dir.path());
    let path = dir.path().join("shuffled.bin");
    let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();

    let offsets = shuffled_offsets(blocks, block_len);
    settle_file_for_read_benchmark(&path);

    let scheduler = MountScheduler::new_for_path(
        StorageRootId::new(),
        dir.path(),
        &SchedulerConfig {
            profile: StorageProfile::Unknown,
            peer_read_concurrency: blocks as usize,
            storage_io: StorageIoConfig {
                peer_read_readahead_bytes: 0,
                peer_read_elevator_budget_ms: 0,
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let elapsed = run_reads(scheduler.clone(), path, &data, &offsets, block_len).await;
    let stats = scheduler.stats();
    let mib = total as f64 / (1024.0 * 1024.0);
    let mib_s = mib / elapsed.as_secs_f64();

    println!(
        "tng_storage_shuffled_baseline blocks={blocks} block_len={block_len} total_mib={mib:.2} elapsed_ms={} mib_s={mib_s:.2} read_ops={} backend_reads={}",
        elapsed.as_millis(),
        stats.read_ops_by_class[IoClass::PeerRead as usize],
        stats.backend_read_ops_by_class[IoClass::PeerRead as usize],
    );

    assert_eq!(stats.read_ops_by_class[IoClass::PeerRead as usize], blocks);
    assert_eq!(
        stats.backend_read_ops_by_class[IoClass::PeerRead as usize],
        blocks
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-device storage benchmark; run explicitly with --ignored --nocapture"]
async fn hdd_peer_read_elevator_reduces_backend_reads_on_shuffled_adjacent_blocks() {
    let blocks = bench_size("TNG_STORAGE_BENCH_BLOCKS", 4096);
    let block_len = bench_size("TNG_STORAGE_BENCH_BLOCK_LEN", 16 * 1024) as usize;
    let total = blocks as usize * block_len;

    let dir = bench_dir();
    print_topology(dir.path());
    let topology = detect_storage_topology(dir.path());
    if topology.profile != StorageProfile::Hdd {
        println!(
            "tng_storage_elevator skipped_non_hdd_profile={:?}",
            topology.profile
        );
        return;
    }

    let path = dir.path().join("elevator-shuffled.bin");
    let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();

    let offsets = shuffled_offsets(blocks, block_len);
    settle_file_for_read_benchmark(&path);

    let scheduler = MountScheduler::new_for_path(
        StorageRootId::new(),
        dir.path(),
        &SchedulerConfig {
            profile: StorageProfile::Unknown,
            peer_read_concurrency: blocks as usize,
            storage_io: StorageIoConfig {
                peer_read_readahead_bytes: 0,
                peer_read_elevator_budget_ms: bench_u64("TNG_STORAGE_ELEVATOR_BUDGET_MS", 250),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let elapsed = run_reads(scheduler.clone(), path, &data, &offsets, block_len).await;
    let stats = scheduler.stats();
    let submitted = stats.read_ops_by_class[IoClass::PeerRead as usize];
    let backend_reads = stats.backend_read_ops_by_class[IoClass::PeerRead as usize];
    let reduction = submitted as f64 / backend_reads.max(1) as f64;
    let mib = total as f64 / (1024.0 * 1024.0);
    let mib_s = mib / elapsed.as_secs_f64();

    println!(
        "tng_storage_elevator blocks={blocks} block_len={block_len} total_mib={mib:.2} elapsed_ms={} mib_s={mib_s:.2} submitted={submitted} backend_reads={backend_reads} reduction={reduction:.2}x batches={} coalesced={}",
        elapsed.as_millis(),
        stats.peer_read_elevator_batches,
        stats.peer_read_elevator_coalesced_requests,
    );

    assert_eq!(submitted, blocks);
    assert!(
        backend_reads * 5 <= submitted,
        "expected >=5x backend read reduction; submitted={submitted} backend_reads={backend_reads}"
    );
}
