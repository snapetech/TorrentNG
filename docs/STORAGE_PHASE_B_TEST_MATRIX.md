# Storage Phase B Test Matrix

This matrix covers the storage topology, automatic preallocation, dedicated
disk workers, hashing pool, peer-read locality, and standalone device elevator
feature set. It is scoped to the Phase B storage work, not the full product
interop matrix.

Run the local automated matrix with:

```sh
scripts/storage_phase_b_matrix.sh
```

Set `STORAGE_PHASE_B_FULL=1` to include the broader engine and API packages
that consume storage behavior:

```sh
STORAGE_PHASE_B_FULL=1 scripts/storage_phase_b_matrix.sh
```

## Automated Matrix

| Area | Coverage | Command |
| --- | --- | --- |
| Storage unit suite | fd pool, positioned I/O, durability, hashing pool, readahead, topology, elevator policy | `cargo test -p rt-storage` |
| Linux topology | mountinfo parsing, `/sys/dev/block` parent device lookup, rotational profile, network mount device ids, CoW filesystem detection | `cargo test -p rt-storage device::tests` |
| Device elevator | HDD queue budget, NVMe pass-through budget, offset ordering, adjacent read coalescing, write non-coalescing, deadline/foreground/choke-critical promotion | `cargo test -p rt-storage elevator::tests` |
| Auto preallocation | `PreallocationMode::Auto` resolves to full only for rotational non-CoW local topology and sparse otherwise | `cargo test -p rt-storage auto_preallocation_policy` |
| Peer read locality | adjacent peer reads return exact requested bytes while reducing backend reads | `cargo test -p rt-metrics storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks -- --nocapture` |
| Runtime isolation | hashing and recheck work do not stall peer-read path | `cargo test -p rt-metrics storage_hash_pool_does_not_block_peer_read_path -- --nocapture` |
| Positioned concurrency | concurrent writes preserve offsets on one cached fd | `cargo test -p rt-metrics storage_positioned_io_preserves_offsets_under_concurrency -- --nocapture` |
| FD bound | file-pool capacity stays bounded under active file churn | `cargo test -p rt-metrics storage_file_pool_stays_bounded_under_active_file_churn -- --nocapture` |
| Engine upload reads | multi-file upload reads still return exact block bytes | `cargo test -p rt-engine upload_block_reads_across_many_file_regions` |
| Pure v2 recheck | taskless v2 recheck still verifies file roots through scheduled storage reads | `cargo test -p rt-engine pure_v2_recheck_verifies_file_roots_without_torrent_task` |

## Full Consumer Matrix

These commands are not storage-only, but they catch regressions in packages
that currently project or depend on storage behavior.

| Area | Command |
| --- | --- |
| Engine | `cargo test -p rt-engine` |
| Native metrics storage scale tests | `cargo test -p rt-metrics storage_ -- --nocapture` |
| qBittorrent facade | `cargo test -p rt-api-qbit` |
| Deluge facade | `cargo test -p rt-api-deluge` |
| Migration importer | `cargo test -p rt-migrate` |

## Manual Platform Matrix

Run these on real target storage before claiming benchmark or production
readiness. The automated unit tests mock topology where practical, but they do
not prove kernel/filesystem performance.

| Platform | Filesystem / mount | Expected profile | Expected preallocation | Required checks |
| --- | --- | --- | --- | --- |
| Linux HDD | `ext4` or `xfs` on rotational block device | `Hdd` | `Full` | `detect_storage_topology` returns parent block `DeviceId`; prepared files allocate before first write; no repeated parent creation in block hot path |
| Linux HDD CoW | `btrfs`, `zfs`, or `bcachefs` on rotational block device | `Hdd` | `Sparse` | `cow = true`; full fallocate is not selected by `Auto` |
| Linux SSD | SATA SSD with rotational flag `0` | `Ssd` | `Sparse` | higher SSD concurrency; elevator budget collapses to pass-through if wired |
| Linux NVMe | `nvme*` parent block device | `Nvme` | `Sparse` | NVMe profile detected by device name even when rotational flag is absent |
| Linux network | `nfs`, `nfs4`, `cifs`, `ceph`, `sshfs`, `virtiofs` | `Network` | `Sparse` | `DeviceId` is stable from mount source; reads never create missing files |
| Linux container overlay | `overlay` or `fuse-overlayfs` | local or `Unknown` | `Sparse` | `cow = true`; no full preallocation fallback spam |
| Non-Linux fallback | any supported local filesystem | `Unknown` unless configured | `Sparse` | explicit `StorageProfile` overrides still drive scheduler concurrency |

## Performance Gates

These gates need benchmark harnesses before they can be marked complete in the
burndown:

| Gate | Target |
| --- | --- |
| FD churn | open/close syscall rate at 10k seeding torrents trends to zero after warmup |
| HDD seed locality | aggregate seed throughput at least 5x non-elevator baseline on the same dataset after scheduler wiring |
| Recheck isolation | long rechecks do not stall peer-read or foreground classes |
| Durability | `clean_shutdown = true` is written only after configured storage sync succeeds |
