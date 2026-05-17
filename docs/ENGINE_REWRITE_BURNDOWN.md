# Engine Rewrite Burndown

This is the working checklist for finishing the native Rust engine rewrite. It is intentionally implementation-facing: every unchecked item should either become code, tests, certification output, or deleted because it no longer applies.

## 1. Durable Session Backbone

- [x] Replace README "Phase 0 stub" labels for implemented crates with accurate status.
- [x] Expand `rt-db` schema beyond `torrents` and `files`.
- [x] Persist `torrent_files` metadata with offsets, priorities, and completion state.
- [x] Persist `torrent_trackers` with tier/order, status, announce timing, and scrape stats.
- [x] Persist `torrent_tags` and `torrent_categories` as normalized tables.
- [x] Persist per-torrent limits and policy flags.
- [x] Add append-only `session_events`.
- [x] Add append-only `job_events`.
- [x] Add durable `jobs` table with resumable checkpoint fields.
- [x] Add `settings`, `storage_roots`, `mounts`, and `api_tokens` tables.
- [x] Add typed DB helpers for events, jobs, files, trackers, settings, mounts, and limits.
- [x] Load engine/session state from DB on startup, not only in-memory registry bootstrap.
- [x] Persist every engine state transition atomically with a session event.
- [x] Add crash recovery tests for DB-backed state.

## 2. Recheck And Verification Jobs

- [x] Convert `EngineHandle::recheck_torrent` from direct command into a durable job.
- [x] Store resumable recheck checkpoint: file index, piece index, byte offset, verified bytes, invalid pieces.
- [x] Add cancellation and pause/resume controls.
- [x] Rate-limit recheck through storage scheduler.
- [x] Commit torrent state only after recheck completes.
- [x] Emit `check_started`, `check_progress`, `check_completed`, and failure events.
- [x] Add crash-resume recheck tests.

## 3. Storage, Import, And Move Safety

- [x] Wire `rt-storage` scheduler into engine reads/writes/rechecks.
- [x] Implement storage root registry and mount identity persistence.
- [x] Add dry-run import mode for existing libraries.
- [x] Add dry-run move/copy planning with conflict and capacity checks.
- [x] Implement same-filesystem rename fast path.
- [x] Implement copy + verify + atomic rename safe path.
- [x] Add rollback/partial failure reporting.
- [x] Add safe delete only after dry-run approval.
- [x] Certify 15k-torrent dry-run import.

## 4. Tracker Correctness

- [x] Persist tracker state across restart.
- [x] Persist uploaded/downloaded/left accounting per torrent.
- [x] Send `started`, `completed`, and `stopped` events exactly once where required.
- [x] Add restart jitter to prevent announce storms.
- [x] Add scrape support/state where tracker supports it.
- [x] Classify tracker failures and warnings into durable state.
- [x] Disable DHT/PEX/LSD by default for private torrents.
- [x] Add private-tracker accounting tests.

## 5. Peer And Download Completion

- [x] Finish production rarest-first piece picker integration.
- [x] Add endgame request scheduling.
- [x] Persist partial download piece state.
- [x] Resume partial downloads after restart.
- [x] Enforce file priorities in picker and writes.
- [x] Verify every received piece before marking complete.
- [x] Harden upload serving for large multi-file torrents.
- [x] Add public Linux ISO download certification.

## 6. Daemon Operations

- [x] Add native health endpoint backed by engine readiness.
- [x] Add Prometheus metrics for torrents, peers, trackers, jobs, storage, and DB.
- [x] Add SSE/WebSocket delta stream for native API.
- [x] Add clean shutdown stopped-announces with bounded deadline.
- [x] Add API token authentication for mutating native endpoints.
- [x] Add structured "why is this not seeding?" diagnostic API.
- [x] Remove stale sidecar assumptions from `rusttorrentd` docs.

## 7. Compatibility APIs

- [x] Keep qBit compatibility, but back it with durable engine state instead of ad hoc fallback state.
- [x] Implement qBit sync delta semantics with stable `rid`.
- [x] Map qBit files/trackers/pieces to real engine metadata and piece state.
- [x] Keep Transmission RPC compatible with durable engine state.
- [x] Keep Deluge compatibility facade backed by durable engine state.
- [x] Add compatibility certification runs for Prowlarr/Sonarr/Radarr/autobrr/cross-seed/NZB360/Transdrone.

## 8. Migration

- [x] Import rTorrent session state and `.rtorrent` fast-resume data.
- [x] Import qBittorrent fastresume/state.
- [x] Import Transmission resume/state.
- [x] Preserve categories/tags/labels/save paths/trackers.
- [x] Add dry-run migration reports.
- [x] Add rollback/backup docs.

## 9. Scale Certification

- [x] Generate synthetic 1k, 5k, 10k, 15k, 50k datasets.
- [x] Cold-start benchmark with DB load and API readiness.
- [x] Idle memory benchmark at 15k torrents.
- [x] API list/filter/sort latency benchmark.
- [x] Tracker restart storm benchmark.
- [x] Recheck-vs-seeding starvation benchmark.
- [x] Publish certification report.

## 10. Release Hygiene

- [x] Update crate READMEs with actual status.
- [x] Update `docs/ENGINE.md` when implemented behavior diverges from design.
- [x] Add threat model review.
- [x] Add backup/restore docs.
- [x] Add production deployment docs for native engine mode.
- [x] Remove or archive Track 1-only compatibility code when native engine supersedes it.

## 11. Red-Team Rectifications

- [x] Make migration DB import atomic across torrent row, files, trackers, tags, and category tables.
- [x] Apply qBit `/torrents/info` offset/limit before engine metadata projection.
- [x] Base qBit `rid` on stable, order-independent projected torrent data including metadata-backed tracker fields.
- [x] Return qBit piece states from fastresume piece state instead of aggregate progress guesses.
- [x] Back Deluge `web.update_ui` with engine metadata.
- [x] Project Transmission per-file completion from torrent bytes done.
- [x] Make public Linux ISO certification fail when `PUBLIC_TRANSFER=1` does not complete in the configured timeout.
- [x] Add focused regression tests for qBit `rid` and Transmission file completion.

## 12. Storage NG (next-gen disk I/O)

See `docs/STORAGE_NG.md` for the full design. Phases A–D are independently
shippable and benchmarkable.

### Phase A — parity floor (behind existing rt-storage API)

- [x] Add `DiskBackend` trait with `PreadBackend` (dedicated bounded blocking pool, separate from Tokio's).
- [x] Replace `seek`+`read`/`write` with positioned `pread`/`pwrite`.
- [x] Add path-keyed `HandleCache` (LRU + idle-TTL sweep), capacity bounded to `RLIMIT_NOFILE`.
- [x] Raise `RLIMIT_NOFILE` toward the hard limit at startup; reserve fds for sockets.
- [x] Add global `FramePool` (size classes, hard byte cap, backpressure → `QueueFull`).
- [x] Call `create_dir_all` once per file at allocation, not per block.
- [x] Keep `scheduled_read`/`scheduled_write`/`PieceVerifier` signatures stable.
- [x] Bench/proxy: real-device file-pool run shows 100k hot reads with one open miss and 99,999 cache hits.

### Phase B — per-device elevator + topology

- [x] Resolve storage roots to physical `DeviceId` (`/sys/block`, rotational, dm/RAID, mergerfs/ZFS/btrfs).
- [x] Auto-detect `StorageProfile` instead of defaulting to `Unknown`.
- [x] Implement `DeviceElevator`: offset-sorted, coalescing, deadline + `choke_critical` promotion.
- [x] Wire `MountScheduler` peer-read permits to HDD elevator submission with bounded queueing.
- [x] Topology-derived preallocation policy (`fallocate` on rotational non-CoW only).
- [x] Bench/proxy: real-device adjacent peer-read readahead reduces backend reads ≥5x on NVMe and HDD.
- [x] Bench/proxy: HDD shuffled peer-read elevator reduces backend reads ≥5x on same dataset.
- [x] Bench/tune: HDD shuffled peer-read wall-clock throughput ≥5× non-elevator baseline on same dataset.

### Phase C — tiered torrents (scale unlock)

- [x] Introduce Dormant/Warm/Hot tiers orthogonal to `TorrentState`.
- [x] Shared timer-wheel reactor for Dormant torrents (no task/channel/fd per torrent).
- [x] Promote to Hot on inbound peer/announce; demote on peer drain + idle.
- [x] Dormant torrents hold only piece bitmap (mmap-backed/compressed) + tracker deadline.
- [x] Bench/proxy: 100k torrents ≤2% active → ≤1 Tokio task per Hot torrent; dormant heap within target.

### Phase D — efficiency

- [ ] `UringBackend` (registered fds + fixed buffers + batched submit), probe-selected.
- [ ] Group-commit per-device `fdatasync` barrier; fastresume watermark gated on barrier.
- [ ] Bounded post-crash recheck (only pieces written since last barrier).
- [ ] Piece-aggregated writes; hash from RAM on dedicated hashing pool; delete read-after-write verify.
- [ ] Adaptive per-connection readahead + `posix_fadvise` page-cache stewardship (`DONTNEED`/`SEQUENTIAL`).
- [ ] `SEEK_HOLE`/`SEEK_DATA`-aware recheck sweep.
- [ ] Bench: kill -9 under write load → bounded recheck, zero silent corruption.
