# rt-storage

`rt-storage` owns storage planning, verification reads, and the per-mount disk
scheduler used by the native engine.

## Architecture

- `MountScheduler` enforces per-`IoClass` concurrency with separate semaphores.
- `StorageIoConfig` controls the disk layer: file-pool size, idle fd TTL,
  worker threads, queue depth, preallocation, durability, and peer-read
  readahead target.
- `scheduled_read` and `scheduled_write` are compatibility wrappers over
  positioned `read_at` and `write_at`.
- A bounded open-file pool avoids per-block fd churn and records hits, misses,
  evictions, idle closes, and current open count.
- Disk operations run on dedicated blocking workers, behind a bounded queue,
  instead of Tokio's shared blocking pool.
- Piece and BEP52 hashing runs on a separate bounded hashing pool.
- `prepare_file` creates parent directories and applies preallocation before
  the first write. The default `Auto` policy resolves from mount/sysfs
  topology: full allocation for rotational non-CoW local storage, sparse
  allocation for SSD/NVMe, network, unknown, and CoW filesystems.
- `sync_data` and `sync_all_open_files` are used by higher layers to make
  fastresume trust conditional on configured durability.
- `StorageIoStats` exposes file-pool, queue, dirty-file, preallocation, sync,
  hash, and bytes-by-class counters for metrics integration.
- `IoClass::PeerRead` can read ahead into a small per-file cache while returning
  exactly the requested block bytes to the caller.

## Correctness Expectations

- Reads use `create(false)` and never create or truncate missing files.
- Writes create files only when the caller explicitly allows creation for a
  known torrent file.
- Positioned I/O validates short reads/writes and returns `StorageError`.
- File-pool keys are normalized absolute paths.
- Preallocation policy is resolved once when the scheduler is created; hot-path
  writes receive a concrete mode.
- Preallocation failures are surfaced before the engine marks blocks or pieces
  valid.

## Tests

Unit tests cover scheduler class isolation, read-missing behavior, write create
policy, fd-pool hit/miss/eviction behavior, sparse preparation, and concurrent
positioned writes on a cached fd. They also assert storage stats for read,
write, sync, and hash work. Engine tests cover upload block assembly across
multi-file regions and fastresume/recheck workflows.

Run:

```sh
cargo test -p rt-storage
cargo test -p rt-engine
```
