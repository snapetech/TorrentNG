# Engine Rewrite Burndown

This is the working checklist for finishing the native Rust engine rewrite. It is intentionally implementation-facing: every unchecked item should either become code, tests, certification output, or deleted because it no longer applies.

## 1. Durable Session Backbone

- [ ] Replace README "Phase 0 stub" labels for implemented crates with accurate status.
- [x] Expand `rt-db` schema beyond `torrents` and `files`.
- [x] Persist `torrent_files` metadata with offsets, priorities, and completion state.
- [x] Persist `torrent_trackers` with tier/order, status, announce timing, and scrape stats.
- [ ] Persist `torrent_tags` and `torrent_categories` as normalized tables.
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
- [ ] Add crash-resume recheck tests.

## 3. Storage, Import, And Move Safety

- [x] Wire `rt-storage` scheduler into engine reads/writes/rechecks.
- [x] Implement storage root registry and mount identity persistence.
- [ ] Add dry-run import mode for existing libraries.
- [ ] Add dry-run move/copy planning with conflict and capacity checks.
- [ ] Implement same-filesystem rename fast path.
- [ ] Implement copy + verify + atomic rename safe path.
- [ ] Add rollback/partial failure reporting.
- [ ] Add safe delete only after dry-run approval.
- [ ] Certify 15k-torrent dry-run import.

## 4. Tracker Correctness

- [x] Persist tracker state across restart.
- [x] Persist uploaded/downloaded/left accounting per torrent.
- [ ] Send `started`, `completed`, and `stopped` events exactly once where required.
- [x] Add restart jitter to prevent announce storms.
- [ ] Add scrape support/state where tracker supports it.
- [x] Classify tracker failures and warnings into durable state.
- [x] Disable DHT/PEX/LSD by default for private torrents.
- [ ] Add private-tracker accounting tests.

## 5. Peer And Download Completion

- [ ] Finish production rarest-first piece picker integration.
- [ ] Add endgame request scheduling.
- [x] Persist partial download piece state.
- [x] Resume partial downloads after restart.
- [x] Enforce file priorities in picker and writes.
- [x] Verify every received piece before marking complete.
- [ ] Harden upload serving for large multi-file torrents.
- [ ] Add public Linux ISO download certification.

## 6. Daemon Operations

- [x] Add native health endpoint backed by engine readiness.
- [x] Add Prometheus metrics for torrents, peers, trackers, jobs, storage, and DB.
- [ ] Add SSE/WebSocket delta stream for native API.
- [ ] Add clean shutdown stopped-announces with bounded deadline.
- [x] Add API token authentication for mutating native endpoints.
- [x] Add structured "why is this not seeding?" diagnostic API.
- [ ] Remove stale sidecar assumptions from `rusttorrentd` docs.

## 7. Compatibility APIs

- [ ] Keep qBit compatibility, but back it with durable engine state instead of ad hoc fallback state.
- [ ] Implement qBit sync delta semantics with stable `rid`.
- [ ] Map qBit files/trackers/pieces to real engine metadata and piece state.
- [ ] Keep Transmission RPC compatible with durable engine state.
- [ ] Keep Deluge compatibility as best-effort facade.
- [ ] Add compatibility certification runs for Prowlarr/Sonarr/Radarr/autobrr/cross-seed/NZB360/Transdrone.

## 8. Migration

- [ ] Import rTorrent session state and `.rtorrent` fast-resume data.
- [ ] Import qBittorrent fastresume/state.
- [ ] Import Transmission resume/state.
- [ ] Preserve categories/tags/labels/save paths/trackers.
- [ ] Add dry-run migration reports.
- [ ] Add rollback/backup docs.

## 9. Scale Certification

- [ ] Generate synthetic 1k, 5k, 10k, 15k, 50k datasets.
- [ ] Cold-start benchmark with DB load and API readiness.
- [ ] Idle memory benchmark at 15k torrents.
- [ ] API list/filter/sort latency benchmark.
- [ ] Tracker restart storm benchmark.
- [ ] Recheck-vs-seeding starvation benchmark.
- [ ] Publish certification report.

## 10. Release Hygiene

- [ ] Update crate READMEs with actual status.
- [ ] Update `docs/ENGINE.md` when implemented behavior diverges from design.
- [ ] Add threat model review.
- [ ] Add backup/restore docs.
- [ ] Add production deployment docs for native engine mode.
- [ ] Remove or archive Track 1-only compatibility code when native engine supersedes it.
