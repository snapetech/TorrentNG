# Native Rust Engine Design

This document covers the native Rust BitTorrent engine. The engine is built
around TorrentNG's universal compatibility goal: import from existing clients,
project the APIs existing tools already speak, interoperate with independent
clients, and keep one durable native model underneath those surfaces.

Track 1 compatibility code remains available as a migration and facade layer,
but native engine state is now the source of truth for torrent rows, files,
trackers, jobs, metrics, and compatibility API projections.

---

## Product shape

**Target persona:** private tracker users, seedbox operators, homelab media automation, large archive seeders, 10k–100k torrent operators, multi-hundred-TB libraries, *arr/autobrr/cross-seed power users, and operators migrating between rTorrent, qBittorrent, Transmission, Deluge, uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati, and generic torrent libraries.

**Not the primary target:** casual desktop torrenting, search-engine plugin users, "download one magnet and watch immediately" users.

The engine is seeding-first and compatibility-first, but the rewrite now
includes native downloading, magnet metadata, DHT/uTP protocol crates, and pure
v2/hybrid metadata support. Streaming remains outside the first production
target.

---

## Why a full rewrite after Track 1

Track 1 solves the immediate pain: broken RPC trust, PHP control plane, polling-based sync, integration fragility. It does not solve the deep problems:

- rTorrent's storage engine has no userspace disk scheduler
- No per-mount queue depth, HDD/SSD profiles, or storage pressure awareness
- No structured event log ("why did this torrent stop seeding?")
- No resumable rechecks with observable progress
- No safe move-and-verify for cross-device operations
- XMLRPC as the internal protocol

Track 2 eliminates all of these at the foundation level.

---

## Crate workspace layout

```
crates/
  rt-bencode/         — bencode parser + canonical encoder, fuzzed, property-tested
  rt-metainfo/        — .torrent and magnet parsing, path sanitization, v1/v2/hybrid infohash
  rt-hash/            — SHA-1 / SHA-256 piece verification, bounded hashing worker pool
  rt-piece-map/       — piece-to-file mapping, request boundary math, invariant tests
  rt-fastresume/      — resume state import (rTorrent .rtorrent, qBit .fastresume)
  rt-storage/         — storage root abstraction, mount awareness, disk scheduler
  rt-tracker/         — HTTP + UDP announce, tiers, backoff, announce accounting, scrape
  rt-peer-wire/       — peer wire codec, extension protocol, fuzzed
  rt-peer-manager/    — connection pool, choking, unchoking, peer scoring, ban rules
  rt-piece-picker/    — rarest-first, endgame, file priority
  rt-dht/             — DHT (Phase 10, private-tracker-off by default)
  rt-utp/             — uTP packet codec, transport state, and async UDP stream primitives (Phase 10)
  rt-session/         — torrent lifecycle types and registry
  rt-db/              — SQLite schema, durable rows, events, jobs, labels, storage roots
  rt-api-model/       — shared API types (serde)
  rt-api-native/      — native REST + WebSocket API (axum)
  rt-api-qbit/        — qBittorrent v2 compatibility shim
  rt-api-transmission/ — Transmission RPC compatibility facade over native state
  rt-api-deluge/      — Deluge compatibility facade over native state
  rt-jobs/            — bulk op job queue, dry-run engine
  rt-metrics/         — Prometheus metrics definitions and scale certification tests
  rt-config/          — TOML config, env override, validation
  rt-migrate/         — import from rTorrent / qBit / Transmission / Deluge / uTorrent / BiglyBT / Tixati / generic libraries
  rt-testkit/         — test fixtures, synthetic torrent generators, interop helpers
  torrentngd/       — binary: wires all modules, signal handling, startup
```

---

## Torrent identity

```rust
enum TorrentId {
    V1 { info_hash: [u8; 20] },
    V2 { info_hash: [u8; 32] },
    Hybrid { v1: [u8; 20], v2: [u8; 32] },
}
```

Never assume SHA-1 forever. v2 and hybrid identity are first-class: APIs,
storage verification, metadata placeholders, fast-resume identity, and
Transmission magnet projection accept 64-character SHA-256 info hashes.

---

## Torrent lifecycle states

```
Imported → MetadataPending → CheckingQueued → Checking → CheckedComplete
                                                       → CheckedPartial → Downloading → Seeding
Seeding → Paused
Seeding → Errored
Seeding → MissingFiles
Seeding → Moving → Seeding
* → Deleting → Retired
```

Every transition is persisted and emits a structured event. No boolean soup.

`TorrentActivityTier` is a separate runtime axis, not another lifecycle state:
`Hot` torrents have active peer/protocol work, `Warm` torrents are idle but
near tracker or recent activity, and `Dormant` torrents retain only compact
state until promotion. This keeps user-visible `TorrentState` stable while
letting the engine scale idle libraries without one task/channel/fd per
torrent. `ActivityTimerWheel` is the shared deadline structure for tier
promotion and idle checks; many torrents share one wheel instead of each idle
torrent owning a timer task.

The Phase C engine primitives are `CompactPieceBitmap`,
`DormantTorrentSnapshot`, and `TierController`. A dormant snapshot keeps the
info hash, lifecycle state, compact MSB-first piece bitmap, tracker deadline,
and last activity timestamp; peer state, open files, channels, request queues,
and piece assemblies are intentionally absent. `TierController` applies inbound
peer, announce, peer-drain, request, state-change, and idle events to the tier
policy and schedules shared idle checks through the timer wheel. `TierScaleSnapshot`
records the release proxy for the 100k-torrent target: at most 2% hot torrents,
at most one active Tokio task per hot torrent, and bounded dormant heap per
torrent.

---

## Storage model

### Design principle

Do not use mmap for torrent data as the primary design. Use an explicit userspace disk scheduler with bounded buffers.

Rationale: libtorrent-rasterbar's own maintainer documented why 2.x mmap was a mistake for I/O control. A 200+ TB seedbox needs deliberate storage scheduling, not OS mmap behavior.

### Storage modes

```
seeding_existing        — verify and seed pre-existing files
download_to_final       — download directly to final path
download_to_temp        — download to temp, move on complete
download_to_category    — use category save root
hardlink_import         — hardlink from source to storage root
copy_import             — copy from source, verify on complete
verify_only             — check without changing state
metadata_only           — no file I/O (metadata/announce only)
```

### Disk scheduler responsibilities

- Mount identity and free space awareness
- Filesystem type detection
- Per-mount queue depth and read/write concurrency limits
- Bounded open-file cache with idle eviction and descriptor budgeting
- Positioned reads/writes; no shared seek cursor in torrent block I/O
- File preparation and preallocation before first download write
- Explicit durability checkpoints before trusting clean fastresume state
- Fastresume durability watermarks track pieces validated since the last
  completed storage barrier. Clean saves advance the barrier after sync;
  unclean saves with a watermark downgrade only those dirty pieces on restart,
  bounding post-crash recheck instead of forcing full-library verification.
- Peer-read locality through internal readahead/coalescing while returning exact requested bytes
- Page-cache stewardship: large peer reads issue `SEQUENTIAL`/`WILLNEED`
  hints, and large recheck reads issue `SEQUENTIAL` before I/O and `DONTNEED`
  after I/O so cold verification sweeps do not evict hot seeding pages.
- Sparse-aware recheck maps data extents with `SEEK_DATA`/`SEEK_HOLE` where
  available, hashes sparse holes as zeroes, and skips reading hole bytes from
  disk. Unsupported filesystems fall back to contiguous reads.
- Completed download pieces are verified from assembled in-memory piece data on
  the dedicated hashing pool whenever the full piece is still buffered; disk
  re-read verification is only the fallback for evicted or oversized pieces.
- HDD vs SSD/NVMe I/O profile
- Sequential vs random pressure awareness
- Priority: recheck < background seeding < active downloads < active streams (future)
- External filesystem pressure hints where available

### Recheck engine

Recheck is not a fire-and-forget operation. It is a first-class job:

```
job_id
torrent_id
file_index (resumable)
piece_index (resumable)
byte_offset (resumable)
verified_bytes
invalid_pieces
started_at / updated_at
state: queued | running | paused | cancelled | complete
```

Requirements:
- Incremental and resumable — survives crash mid-check
- Rate-limited and per-mount throttled
- Cancellable without data loss
- Visible through API and event log
- Verifies files without changing torrent state until commit

### File movement

Moving 200+ TB is a database migration, not a file copy:

1. Dry-run: conflict detection, capacity check, path mapping preview
2. Fast path: same-filesystem rename
3. Safe path: copy + verify SHA + atomic rename + cleanup
4. Failure: partial failure report, rollback plan
5. Post-move: tracker path update, no silent deletes

---

## Networking

### Peer connection manager

- TCP listener
- uTP packet codec, connection-state support, async UDP stream primitives,
  shared incoming endpoint demux, opt-in incoming and outgoing peer-wire paths,
  and metadata-fetch support; runtime capability reports whether any uTP
  transport path is enabled by current policy
- Outgoing connection queue with backpressure
- Per-torrent and global peer caps
- Peer scoring and ban/eject rules
- Choking/unchoking scheduler (no fibrillation)
- Request pipeline and stale peer cleanup
- Extension protocol negotiation

### Protocol targets

| BEP | Description | Status |
|---|---|---|
| BEP 3 | BitTorrent v1 baseline | implemented |
| BEP 9 | Metadata exchange / magnet | implemented for v1 metadata from tracker or DHT-discovered peers; pure v2 placeholders/completion are taskless |
| BEP 10 | Extension protocol | implemented |
| BEP 11 | PEX | compatibility policy present; private torrents disable peer discovery by default |
| BEP 12 | Multitracker | implemented |
| BEP 14 | LSD | private torrents disable local discovery by default |
| BEP 15 | UDP trackers | implemented for v1; v2 UDP announces are rejected explicitly |
| BEP 23 | Compact peer list | implemented |
| BEP 27 | Private torrents | implemented |
| BEP 29 | uTP | implemented for packet/state/UDP stream primitives plus opt-in engine peer-wire and metadata paths; public interop remains release evidence |
| BEP 32 | IPv6 | partial: tracker compact IPv6 peers and DHT compact IPv6 peer values are parsed/forwarded; DHT routing nodes remain IPv4-only |
| BEP 52 | BitTorrent v2 / hybrid | implemented for parsing, identity, metadata projection, storage root verification, fastresume, and compatibility projections |

DHT is implemented in Phase 10. Private-tracker profiles disable DHT/PEX/LSD by default.

---

## Tracker subsystem

Track per-torrent per-tracker:
```
uploaded / downloaded / left
event (started/stopped/completed)
last_announce / next_announce / last_success
failure_reason / warning_message
seeders / leechers / completed
```

For private trackers, accounting correctness is not optional. Ratio bugs kill trust.

Restart behavior:
- Jitter announces across the restart window
- Never announce-storm (no simultaneous burst of 15k started events)
- Send stopped announces on clean shutdown within configured time budget
- Crash recovery resumes state from DB, no phantom completed events

---

## Session database

SQLite initially. Clean abstraction so Postgres is an option later.

Minimum tables:
```sql
torrents
torrent_files
torrent_trackers
torrent_tags
torrent_categories
torrent_limits
jobs
job_events
session_events        -- append-only event log
mounts
storage_roots
api_tokens
settings
```

### Event log

Append-only, never delete. Answers "why did this torrent stop seeding?":

```
torrent_added / metadata_resolved
check_started / check_progress / check_completed
tracker_announce_started / failed / succeeded
peer_connected / disconnected
piece_verified
file_missing / path_conflict / permission_error
move_started / move_completed / move_failed
error_raised
```

---

## API design

### Native API

```
/api/v1/torrents
/api/v1/torrents/{id}
/api/v1/torrents/{id}/files
/api/v1/torrents/{id}/trackers
/api/v1/torrents/{id}/peers
/api/v1/torrents/{id}/pieces
/api/v1/jobs
/api/v1/events          (SSE or WebSocket)
/api/v1/mounts
/api/v1/settings
/api/v1/settings/user-agent
/api/v1/health
/api/v1/metrics
```

Principles:
- REST for CRUD and bulk ops
- SSE/WebSocket for event stream (no polling)
- OpenAPI spec generated in CI
- Typed error codes, not freeform strings
- Idempotency keys for destructive operations
- All destructive bulk ops support `dry_run=true`

### qBittorrent compatibility API

Compatibility shim is a translation layer over the native model. qBit API quirks do not leak into the engine.

Priority 1 (Phase 6):
```
/api/v2/auth/login|logout
/api/v2/app/version|webapiVersion|preferences
/api/v2/sync/maindata
/api/v2/transfer/info
/api/v2/torrents/info|add|pause|resume|delete|recheck|reannounce
/api/v2/torrents/files|trackers|filePrio|setCategory|addTags|removeTags
```

Priority 2 (Phase 9):
```
/api/v2/torrents/setLocation|renameFile|renameFolder
/api/v2/torrents/categories|tags
share limits, speed limits
```

Priority 3 (Phase 12):
```
RSS, search, cookies, advanced preferences
```

### Transmission compatibility API

```
/transmission/rpc
session_get / session_set
torrent_get / torrent_add / torrent_set
torrent_start / torrent_stop / torrent_remove
free_space
```

Transmission `magnetLink` projection emits `btih` for v1 hashes and BEP 52
`btmh` multihash links for pure v2 hashes.

---

## Security model

### API

- No unauthenticated mutating endpoints
- API tokens with scopes
- Bearer token mode for automation
- Browser cookie mode with CSRF protection
- Reverse-proxy safe (trust X-Remote-User if configured)
- Local socket option
- Audit log for destructive operations

### Filesystem

- Reject absolute paths from torrent metadata
- Reject `..` path components
- Sanitize platform-reserved names
- Detect symlink traversal
- Never write outside assigned storage roots
- Dry-run all moves and imports
- No arbitrary script execution by default
- Explicit allowlist for URL schemes when fetching .torrent URLs

### Network

- All parsers fuzzed (bencode, metainfo, tracker responses, peer wire, magnet)
- Bounded message sizes
- Per-peer request rate limits
- Ban flood/invalid request patterns
- No DHT on private torrents by default
- SSRF protection for .torrent URL fetching

---

## Observability

### Structured logs (tracing)

Every significant event is a structured JSON log line with consistent fields:
```json
{ "event": "tracker_announce_failed", "torrent_id": "...", "tracker": "...", "reason": "...", "next_retry_secs": 300 }
```

### Prometheus metrics

```
torrentng_torrents_total{state}
torrentng_peers_connected
torrentng_tracker_announces_total{result}
torrentng_disk_queue_depth{mount}
torrentng_disk_bytes_total{direction, mount}
torrentng_recheck_bytes_total
torrentng_api_request_duration_seconds{endpoint, method}
torrentng_event_lag_seconds
torrentng_job_queue_depth{type}
```

### Diagnostics API

Every torrent answers these questions through the API:
- Why is this not seeding?
- Why is this tracker failing?
- Why did this recheck start?
- Why is this file missing?
- Why is this torrent paused?
- Why is this torrent not announcing?
- Why did this move fail?

---

## Testing strategy

### Fuzz targets (from day one)

- `.torrent` parser
- Magnet URI parser
- Bencode parser
- Tracker HTTP response parser
- Tracker UDP packet codec
- Peer wire message parser
- Resume DB import
- qBit API input shapes

### Property tests

- Bencode round-trip
- Invalid bencode rejection
- Path traversal rejection (all variants)
- Piece-to-file map invariants
- Chunk request boundary math
- Tracker response parser robustness

### Failure injection tests

- SIGKILL during recheck
- SIGKILL during file move
- SIGKILL during DB transaction
- Disk full
- Permission denied
- Tracker timeout / invalid response
- Corrupted piece
- Missing / renamed file
- Mount disappears
- Network outage / DNS failure

### Scale tests (synthetic)

```
1k / 5k / 10k / 15k / 50k / 100k torrents
Large single-file torrents
Large multi-file deep directory trees
Many small files
Multiple storage roots
Multiple tracker tiers
```

---

## Migration

Implemented migration support lives in `rt-migrate`. Scanners are read-only
against source session directories and produce an auditable dry-run plan before
anything is written to the native DB. The apply path writes native torrent rows,
file rows, tracker rows, labels, categories, counters, ratio, and completion
state through `rt-db`.

### From rTorrent

- Import session directory and `.torrent` files
- Import resume state (no forced global recheck for complete torrents)
- Import labels, views, save paths, tracker lists
- Ratio/upload stats where available
- Always dry-run first, never destructive

### From qBittorrent

- Import `BT_backup/` directory
- Import `.fastresume` files
- Categories, tags, save paths, torrent states, trackers

### From Transmission

- Import resume files and torrents directory
- Labels/groups and download directories

### Migration acceptance criteria

A migration is not done until:
- Pre-existing complete files seed without overwrite confusion
- Imported complete torrents do not recheck unless requested
- Missing paths are reported clearly with path-remap preview
- Failed import is resumable
- Dry-run output is human-readable

---

## Runtime

- Tokio async runtime (peer sockets, trackers, API, events)
- `bytes` for network buffers
- `rusqlite` (bundled) for session DB
- `axum` + `tower` for HTTP API
- `tracing` + `tracing-subscriber` JSON layer for logs
- Rust integration tests for migration, scale, compatibility APIs, tracker
  behavior, storage scheduling, and crash recovery
- Certification reports under `certification/reports/`

No global shared mutable state. Strict actor/task boundaries. Bounded channels with backpressure everywhere.

## Release Operations

- Backup/restore: [BACKUP_RESTORE.md](BACKUP_RESTORE.md)
- Native deployment: [NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md)
- Threat model: [THREAT_MODEL.md](THREAT_MODEL.md)
- Certification report: `scripts/native_engine_certification_report.sh`
- Public Linux ISO certification: `scripts/public_linux_iso_certification.sh`
