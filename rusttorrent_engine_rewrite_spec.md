# RustTorrent Engine Rewrite Spec

Date: 2026-05-15
Target audience: implementation agents, including weaker/free coding models
Primary goal: rTorrent-class Rust torrent engine/control-plane for very large headless seed libraries

---

## 0. Executive Summary

Build a Rust-native BitTorrent daemon optimized for:

- 10,000 to 15,000+ torrents as a normal operating target
- 50,000+ torrents as a tested stretch target
- 200+ TB storage libraries
- private-tracker correctness
- safe import of pre-existing files
- reliable long-term seeding
- qBittorrent-compatible API for ecosystem compatibility
- native OpenAPI/JSON API for first-class control
- Transmission-style RPC discipline
- event-driven state synchronization
- operational diagnostics
- per-mount storage scheduling
- resumable, throttled rechecking
- crash-safe metadata/session persistence

Do not start by cloning rTorrent's internal architecture. Start with a clean Rust engine/runtime/library/SDK architecture that preserves rTorrent's high-scale seeding intent but replaces XMLRPC/SCGI/PHP-era assumptions with typed APIs, durable state, explicit storage scheduling, and testable protocol modules.

---

## 1. Non-Negotiable Product Requirements

### 1.1 Scale

Hard target for first serious release:

- Load 15,000 torrents.
- Index 200+ TB of torrent payload paths.
- Cold start under 120 seconds on a modern NVMe-backed metadata DB.
- Idle daemon RSS under 2.5 GB at 15k torrents.
- UI/API list/filter/sort must not require full engine scans.
- Recheck operations must be queued, throttled, resumable, and cancellable.
- Restart must not cause tracker announce storms.
- Crash recovery must not force global recheck.
- Storage operations must be dry-runnable before destructive changes.

Stretch target:

- 50,000 torrents.
- 500+ TB library.
- 100,000 metadata-only synthetic torrents for state/index testing.

### 1.2 Workload Bias

Primary:

- seeding existing files
- private trackers
- import from existing rTorrent/qBittorrent/Transmission installs
- *arr, Prowlarr, autobrr, cross-seed, NZB360/Transdrone style compatibility
- headless server deployment

Secondary:

- downloading public torrents
- DHT/PEX/LSD
- streaming
- desktop GUI

Do not optimize early architecture around streaming or casual desktop usage.

---

## 2. Core Architecture

### 2.1 Process Model

Initial product is one daemon process:

```text
rusttorrentd
├── native API server
├── qBittorrent compatibility API
├── optional Transmission compatibility API
├── SSE/WebSocket event stream
├── session supervisor
├── torrent actors
├── tracker manager
├── peer listener + peer connection manager
├── piece picker
├── disk scheduler
├── verification/recheck workers
├── session DB
├── job queue
├── metrics/logging/diagnostics
└── optional static WebUI
```

Keep it single-process until scaling evidence justifies splitting. Use internal crate boundaries so components can be split later.

### 2.2 Runtime

Use Tokio for async networking, timers, channels, task management, tracker HTTP/UDP, peer sockets, API, and event streaming.

Rules:

- Never run hashing on core async reactor tasks.
- Never run blocking filesystem scans directly on Tokio worker threads.
- Use bounded blocking pools for hashing and verification.
- Use bounded queues everywhere.
- Backpressure is mandatory.
- Any unbounded channel must be treated as a bug unless explicitly justified in code comments and tests.

Recommended runtime primitives:

- `tokio::net::TcpListener`
- `tokio::net::TcpStream`
- `tokio::net::UdpSocket`
- bounded `mpsc`
- `watch` for latest config/state snapshots
- `broadcast` only for small event fan-out, never bulk torrent state
- `CancellationToken`
- task naming via `tracing`
- `JoinSet` for controlled fan-out
- explicit task supervisors

### 2.3 Repository Layout

```text
rusttorrent/
├── Cargo.toml
├── crates/
│   ├── rt-bencode/
│   ├── rt-metainfo/
│   ├── rt-hash/
│   ├── rt-path/
│   ├── rt-storage/
│   ├── rt-piece-map/
│   ├── rt-fastresume/
│   ├── rt-tracker/
│   ├── rt-peer-wire/
│   ├── rt-peer-manager/
│   ├── rt-piece-picker/
│   ├── rt-dht/
│   ├── rt-utp/
│   ├── rt-session/
│   ├── rt-db/
│   ├── rt-jobs/
│   ├── rt-api-model/
│   ├── rt-api-native/
│   ├── rt-api-qbit/
│   ├── rt-api-transmission/
│   ├── rt-metrics/
│   ├── rt-config/
│   ├── rt-migrate/
│   ├── rt-testkit/
│   └── rusttorrentd/
├── web/
├── docs/
├── deploy/
│   ├── systemd/
│   ├── docker/
│   ├── compose/
│   └── kubernetes/
├── benches/
├── fuzz/
└── testdata/
```

---

## 3. Agent Implementation Rules

These are written for weaker implementation models.

### 3.1 Global Rules

1. Do not invent protocol behavior. Read the BEP or local spec file first.
2. Do not add dependencies without updating `docs/dependencies.md`.
3. Do not use unsafe Rust unless the crate-level README contains a written justification and tests.
4. Do not use mmap for torrent payload data in v1.
5. Do not expose any mutating API without authentication.
6. Do not implement delete/move operations without dry-run mode first.
7. Do not implement a "torrent complete" transition unless all pieces are verified.
8. Do not perform path joins from torrent metadata without passing through `rt-path`.
9. Do not make qBittorrent compatibility structs the internal engine model.
10. Do not use a global mutex around session state.
11. Do not use unbounded queues for peer piece data.
12. Do not make recheck an inline API request. It must be a job.
13. Do not silently skip corrupt metadata; return typed errors.
14. Do not rely on wall-clock time for deterministic tests; inject clock.
15. Do not parse bencode with generic JSON/YAML/TOML libraries.

### 3.2 Required Per-PR Checklist

Every implementation PR must include:

- unit tests
- at least one negative test
- tracing events where operationally relevant
- typed error variants
- no `unwrap()`/`expect()` in library code except tests
- no blocking I/O in async tasks unless isolated and documented
- README update for public crate APIs
- acceptance criteria copied into PR body

---

## 4. Crate Specifications

## 4.1 `rt-bencode`

Purpose: canonical bencode parser/encoder.

Must support:

- bytes
- integers
- lists
- dictionaries
- borrowed parse mode
- owned parse mode
- canonical dictionary key validation
- exact byte-span capture for info dictionary hashing
- rejection of invalid integers like `i-0e` and `i03e`
- rejection of unsorted dictionary keys in strict mode
- configurable recursion/depth limit
- configurable max string length
- fuzz targets

Public API sketch:

```rust
pub enum BValue<'a> {
    Bytes(&'a [u8]),
    Int(i128),
    List(Vec<BValue<'a>>),
    Dict(Vec<(&'a [u8], BValue<'a>)>),
}

pub struct ParseOptions {
    pub strict: bool,
    pub max_depth: usize,
    pub max_string_len: usize,
}

pub struct ParsedBencode<'a> {
    pub root: BValue<'a>,
}

pub fn parse(input: &[u8], options: ParseOptions) -> Result<ParsedBencode<'_>, BencodeError>;
pub fn encode_canonical(value: &OwnedBValue) -> Vec<u8>;
```

Tests:

- valid string/int/list/dict
- invalid leading zero
- invalid negative zero
- invalid unsorted dict
- invalid trailing bytes
- deeply nested input rejected
- random roundtrip property tests
- fuzz parser never panics

Acceptance:

- computes BEP 3 infohash from exact original info substring when parsing `.torrent`
- rejects invalid metadata instead of decode/re-encode hashing invalid input

## 4.2 `rt-metainfo`

Purpose: parse torrent metainfo into typed v1/v2/hybrid structures.

Types:

```rust
pub enum TorrentMeta {
    V1(TorrentMetaV1),
    V2(TorrentMetaV2),
    Hybrid(TorrentMetaHybrid),
}

pub struct TorrentMetaV1 {
    pub info_hash: [u8; 20],
    pub announce: Option<String>,
    pub announce_list: Vec<Vec<String>>,
    pub name: String,
    pub piece_length: u64,
    pub pieces: Vec<[u8; 20]>,
    pub files: Vec<TorrentFileV1>,
    pub private: bool,
}

pub struct TorrentFileV1 {
    pub index: u32,
    pub length: u64,
    pub path: SafeRelPath,
    pub offset: u64,
}
```

Requirements:

- v1 parsing first
- v2 parser scaffold early, implementation later
- hybrid identity support in public model from day one
- preserves raw metainfo bytes
- exposes tracker tiers
- exposes private flag
- validates file paths through `rt-path`
- rejects absolute paths, `..`, empty components, NULs
- handles zero-length files explicitly

Tests:

- single file torrent
- multi-file torrent
- private torrent
- announce-list tiers
- zero-length file
- path traversal rejection
- invalid pieces length rejection
- invalid piece length rejection
- invalid UTF-8 policy documented and tested

Acceptance:

- returns stable typed metadata for valid v1 torrents
- never writes or plans unsafe paths

## 4.3 `rt-path`

Purpose: all path safety and storage-root-relative planning.

Core types:

```rust
pub struct SafeRelPath(Vec<PathComponent>);
pub struct StorageRootId(Uuid);
pub struct StorageRoot {
    pub id: StorageRootId,
    pub path: PathBuf,
    pub min_free_bytes: u64,
    pub profile: StorageProfile,
}

pub enum StorageProfile {
    Hdd,
    Ssd,
    Nvme,
    Network,
    Unknown,
}
```

Rules:

- no absolute torrent paths
- no parent traversal
- no NUL
- no platform-reserved names
- no symlink escape during real file open
- always resolve against configured storage root
- support path mapping dry-runs

Tests:

- `../evil` rejected
- `/absolute` rejected
- Windows reserved names rejected when compatibility mode enabled
- symlink escape detected in integration tests
- long path behavior documented

Acceptance:

- all storage writes require `StorageRoot + SafeRelPath`

## 4.4 `rt-piece-map`

Purpose: piece/file mapping and chunk mapping.

Must provide:

- global torrent byte offset to file spans
- piece index to byte ranges
- request `(piece, begin, length)` validation
- file priority mapping
- wanted/unwanted piece calculation
- sparse file planning
- last-piece truncation handling

Types:

```rust
pub struct PieceMap {
    pub piece_length: u64,
    pub total_length: u64,
    pub piece_count: u32,
    pub files: Vec<FileSpan>,
}

pub struct FileSpan {
    pub file_index: u32,
    pub path: SafeRelPath,
    pub offset: u64,
    pub length: u64,
}
```

Tests:

- single-file piece mapping
- multi-file piece crossing boundaries
- zero-length file
- last piece shorter than piece length
- invalid request > 16 KiB rejected in BEP3-compatible mode
- property tests for offset coverage

Acceptance:

- no off-by-one on piece/file boundaries

## 4.5 `rt-storage`

Purpose: explicit disk I/O scheduler and file access layer.

Do not use mmap in v1.

Responsibilities:

- per-mount queue
- bounded read concurrency
- bounded write concurrency
- foreground/background job classes
- piece reads
- piece writes
- verification reads
- move/copy operations
- free-space checks
- crash-safe staged moves
- file open cache with limits
- optional sparse preallocation
- optional platform-specific I/O priority hooks

Core concepts:

```rust
pub enum IoClass {
    PeerRead,
    PeerWrite,
    Recheck,
    MoveCopy,
    Metadata,
    Foreground,
}

pub struct IoRequest {
    pub class: IoClass,
    pub storage_root: StorageRootId,
    pub file_index: u32,
    pub offset: u64,
    pub len: usize,
}
```

Scheduler requirements:

- Peer reads must not be starved by bulk recheck.
- Recheck must not saturate HDD queue.
- Move/copy must be pauseable.
- All operations must emit metrics.
- All operations must have cancellation path.

Tests:

- queued reads complete
- per-mount limits enforced
- recheck throttling does not block peer read beyond SLA
- disk full surfaces typed error
- permission denied surfaces typed error
- interrupted staged move can resume or roll back

Acceptance:

- can verify and seed existing files without creating/truncating them accidentally

## 4.6 `rt-fastresume`

Purpose: durable verification/session state.

Must store:

- torrent identity
- metainfo fingerprint
- storage root
- safe path mapping
- per-piece verification state
- per-file size/mtime/inode hints
- last full verification time
- dirty flags
- tracker accounting counters
- session generation

Rules:

- fastresume is an optimization, not source of truth
- if file hints mismatch, mark affected ranges unknown
- never mark pieces valid without hash verification or trusted import policy
- import policy must be explicit

Tests:

- valid fastresume reload
- changed mtime invalidates affected file pieces
- changed size invalidates affected file pieces
- missing file marks pieces unknown/missing
- corrupted DB handled gracefully

Acceptance:

- crash recovery does not force global recheck unless integrity cannot be established

## 4.7 `rt-tracker`

Purpose: HTTP/UDP tracker announce and scrape.

Responsibilities:

- tracker tier handling
- private tracker rules
- HTTP announce
- UDP announce
- compact peer parsing
- non-compact peer parsing
- started/stopped/completed events
- retry/backoff
- per-tracker state
- jitter
- announce storm prevention
- scrape support
- error classification

Types:

```rust
pub enum TrackerEvent {
    Started,
    Stopped,
    Completed,
    Empty,
}

pub enum TrackerStatus {
    NeverAnnounced,
    Announcing,
    Working,
    Warning(String),
    Error(TrackerError),
    Disabled,
}

pub struct AnnounceRequest {
    pub info_hash: InfoHash,
    pub peer_id: [u8; 20],
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: TrackerEvent,
    pub compact: bool,
}
```

Private mode:

- If torrent `private=1`, do not use DHT, PEX, LSD by default.
- If torrent `private=1`, only use peers returned by the selected private tracker tier.
- Tracker tier switching must follow BEP private rules.
- On tier switch for private torrent, disconnect existing peers unless policy says otherwise and tests prove compliance.

Tests:

- HTTP announce success
- HTTP failure reason
- compact peers
- UDP connect/announce success
- timeout/backoff
- stopped event on shutdown within budget
- private tracker disables DHT/PEX/LSD
- announce jitter prevents 15k torrent burst

Acceptance:

- private tracker accounting is correct enough to trust with ratio-sensitive trackers

## 4.8 `rt-peer-wire`

Purpose: BEP 3 peer protocol codec and state machine primitives.

Must implement:

- handshake encode/decode
- keepalive
- choke
- unchoke
- interested
- not interested
- have
- bitfield
- request
- piece
- cancel
- extension message placeholder
- strict request validation

Rules:

- hard cap message length
- reject request larger than configured block size
- reject invalid piece index
- reject begin/length outside piece
- no allocation proportional to peer-controlled length unless below hard cap
- parser must be fuzzed

Tests:

- valid handshake
- wrong protocol string rejected
- wrong infohash rejected
- bitfield length validated
- request size validated
- malformed frame rejected without panic

Acceptance:

- codec can interoperate with qBit/Transmission in a local test swarm

## 4.9 `rt-peer-manager`

Purpose: manage live peer connections.

Responsibilities:

- listener
- outbound dialer
- peer lifecycle
- per-peer state
- per-torrent peer caps
- global peer caps
- choked/interested state
- upload slot scheduler
- optimistic unchoke
- request queue
- bandwidth accounting
- peer disconnect reasons
- backpressure to storage

Seeding-first MVP:

- accept inbound
- handshake
- bitfield/have-all
- handle interested
- unchoke according to upload slot policy
- serve valid piece requests
- keep accounting

Downloading comes later.

Tests:

- one peer downloads piece
- multiple peers request same piece
- invalid request disconnects or rejects per policy
- upload slots capped
- peer cleanup releases resources
- storage read failure propagates correctly

Acceptance:

- can seed a complete torrent to another mainstream client

## 4.10 `rt-piece-picker`

Purpose: piece selection for downloading.

Phase 1: stub for seeding-only.

Phase 2:

- rarest-first
- randomization
- priority files
- endgame mode
- avoid duplicate requests unless endgame
- cancel duplicates after piece completes
- partial file download
- sequential mode optional but not default for general use

Tests:

- rarest selection
- no unwanted pieces selected
- endgame sends duplicates only when all missing blocks pending
- cancel duplicates after completion

## 4.11 `rt-session`

Purpose: top-level torrent/session state machine.

Torrent states:

```text
Imported
MetadataPending
CheckingQueued
Checking
CheckedComplete
CheckedPartial
Downloading
Seeding
Paused
Errored
MissingFiles
Moving
Deleting
Retired
```

Transition rules:

- `Imported -> CheckingQueued`
- `CheckingQueued -> Checking`
- `Checking -> CheckedComplete`
- `Checking -> CheckedPartial`
- `CheckedComplete -> Seeding`
- `CheckedPartial -> Downloading`
- any active state -> Paused
- any state -> Errored
- Moving only through job queue
- Deleting only through job queue

Every transition:

- validates preconditions
- persists to DB
- emits event
- updates metrics

Tests:

- legal transitions
- illegal transitions rejected
- crash during transition recovers to safe state
- event emitted once

Acceptance:

- no boolean soup for torrent lifecycle

## 4.12 `rt-db`

Purpose: SQLite persistence.

Use SQLite first.

Tables:

```sql
torrents
torrent_files
torrent_trackers
torrent_pieces
torrent_tags
torrent_categories
torrent_limits
peers
tracker_stats
jobs
job_events
session_events
mounts
storage_roots
api_tokens
settings
schema_migrations
```

Rules:

- migrations are versioned
- every write is transactional
- no giant per-piece row explosion unless benchmark says OK
- piece state may use compressed bitsets/blobs
- event log retention configurable
- DB backup API supported

Tests:

- migrations up from empty
- crash-safe transaction behavior
- 15k torrent insert benchmark
- DB reload benchmark
- corruption handling path

Acceptance:

- 15k torrents load from DB under target

## 4.13 `rt-jobs`

Purpose: long-running operations.

Job types:

```text
ImportTorrent
VerifyTorrent
RecheckTorrent
MoveTorrent
CopyTorrent
DeleteTorrent
BulkSetCategory
BulkSetTags
BulkEditTrackers
BulkReannounce
BulkPause
BulkResume
```

Job fields:

```text
job_id
kind
state
created_at
started_at
updated_at
finished_at
progress
dry_run
cancellable
error
affected_torrents
```

Rules:

- All destructive jobs must support dry-run.
- Recheck/move/copy must support cancellation.
- Job progress must persist.
- Jobs must emit events.
- API starts jobs and returns job ID.

Tests:

- dry-run bulk tracker edit
- cancel recheck
- resume interrupted recheck
- move failure rolls back or pauses safely
- delete never removes files unless explicit flag set

## 4.14 `rt-api-native`

Purpose: first-class API.

Endpoints:

```text
GET    /api/v1/health
GET    /api/v1/version
GET    /api/v1/torrents
POST   /api/v1/torrents
GET    /api/v1/torrents/{id}
POST   /api/v1/torrents/{id}/pause
POST   /api/v1/torrents/{id}/resume
POST   /api/v1/torrents/{id}/recheck
GET    /api/v1/torrents/{id}/files
GET    /api/v1/torrents/{id}/trackers
GET    /api/v1/torrents/{id}/peers
GET    /api/v1/jobs
GET    /api/v1/jobs/{id}
POST   /api/v1/jobs/{id}/cancel
GET    /api/v1/events
GET    /api/v1/storage-roots
POST   /api/v1/storage-roots
GET    /api/v1/settings
PATCH  /api/v1/settings
```

Event stream:

```text
GET /api/v1/events/stream
```

Use SSE first. WebSocket optional later.

Rules:

- API models are not engine internals.
- All mutating endpoints require auth.
- All bulk mutation endpoints support dry-run.
- Pagination is mandatory for torrent lists.
- Filtering/sorting done server-side.
- No endpoint returns 15k full torrent details by default.

## 4.15 `rt-api-qbit`

Purpose: qBittorrent compatibility.

Implement priority endpoints:

```text
/api/v2/auth/login
/api/v2/auth/logout
/api/v2/app/version
/api/v2/app/webapiVersion
/api/v2/app/preferences
/api/v2/sync/maindata
/api/v2/transfer/info
/api/v2/torrents/info
/api/v2/torrents/add
/api/v2/torrents/pause
/api/v2/torrents/resume
/api/v2/torrents/delete
/api/v2/torrents/recheck
/api/v2/torrents/reannounce
/api/v2/torrents/properties
/api/v2/torrents/trackers
/api/v2/torrents/files
/api/v2/torrents/filePrio
/api/v2/torrents/setCategory
/api/v2/torrents/addTags
/api/v2/torrents/removeTags
```

Compatibility policy:

- Match qBit response shapes where possible.
- Maintain compatibility matrix.
- Return documented qBit-like status codes where needed.
- Do not implement search/RSS until core is stable.
- Sonarr/Radarr/Prowlarr/autobrr compatibility is the real acceptance gate.

Tests:

- mock *arr add torrent workflow
- login/session cookie
- `sync/maindata` delta behavior
- add magnet
- add torrent file
- pause/resume/delete/recheck/reannounce
- tags/categories

## 4.16 `rt-api-transmission`

Lower priority.

Implement later:

```text
/transmission/rpc
session_get
session_set
torrent_get
torrent_add
torrent_set
torrent_start
torrent_stop
torrent_remove
free_space
```

Use JSON-RPC 2.0 style.

## 4.17 `rt-migrate`

Purpose: import from existing clients.

Importers:

```text
rTorrent session directory
ruTorrent metadata where available
qBittorrent BT_backup
Transmission resume/torrents directories
generic folder + torrent files
```

Rules:

- import is always dry-run first
- path remapping supported
- missing files reported
- conflicts reported
- no overwrite by default
- no delete during import
- import report persisted

Acceptance:

- can import 15k torrents from folder of `.torrent` files + path mapping
- can mark complete files as candidate seeding after verification

---

## 5. API/SDK Design

### 5.1 Rust SDK

Expose a Rust SDK crate:

```rust
use rt_sdk::{Client, TorrentAdd, TorrentId};

let client = Client::connect("http://localhost:8080")
    .bearer_token(token)
    .build()?;

let job = client.torrents()
    .add(TorrentAdd::from_file("ubuntu.torrent")
    .storage_root("media")
    .paused(true))
    .await?;
```

SDK modules:

```text
client
torrents
jobs
events
storage
settings
qbit_compat
models
errors
```

### 5.2 CLI

Binary:

```text
rtctl
```

Commands:

```text
rtctl status
rtctl torrent list
rtctl torrent add FILE --root media --paused
rtctl torrent pause HASH
rtctl torrent resume HASH
rtctl torrent recheck HASH
rtctl job list
rtctl job watch JOB_ID
rtctl import rtorrent --session PATH --dry-run
rtctl storage list
rtctl diagnostics torrent HASH
```

Output modes:

```text
table
json
yaml
```

---

## 6. Test Harness

### 6.1 Local Swarm Testkit

`rt-testkit` must support:

- fake HTTP tracker
- fake UDP tracker
- fake peer
- temporary storage roots
- deterministic clock
- packet capture hooks
- corrupt piece injector
- slow disk simulator
- tracker timeout simulator
- restart/crash harness

### 6.2 Interop Tests

Run against:

- qBittorrent
- Transmission
- rTorrent if available
- rqbit if useful

Test:

- RustTorrent seeds, qBit downloads
- qBit seeds, RustTorrent downloads
- RustTorrent announces to fake tracker correctly
- RustTorrent qBit API works with mocked *arr calls

### 6.3 Fuzz Targets

Required:

```text
fuzz_bencode_parse
fuzz_metainfo_parse
fuzz_peer_wire_frame
fuzz_tracker_response
fuzz_magnet_parse
fuzz_resume_import
```

---

## 7. Benchmarks

### 7.1 Scale Dataset Generator

Create `rt-benchgen`.

Can generate:

- torrent metadata for 1k/10k/15k/50k torrents
- synthetic file trees
- mixed single-file/multi-file torrents
- realistic media library path distributions
- large torrent sizes without allocating payload
- tracker tier patterns
- private/public flags

### 7.2 Required Benchmarks

```text
cold_start_1k
cold_start_10k
cold_start_15k
cold_start_50k
idle_memory_15k
db_load_15k
api_list_15k
api_filter_15k
api_sort_15k
qbit_sync_maindata_15k
tracker_schedule_15k
announce_jitter_15k
recheck_1tb_hdd_sim
recheck_1tb_nvme_sim
storage_peer_read_under_recheck
crash_recovery_mid_recheck
crash_recovery_mid_move
```

### 7.3 Release Benchmark Report

Every release candidate must publish:

```text
version
commit
host CPU/RAM/storage
torrent count
library size
cold start time
idle RSS
API p50/p95/p99
qBit sync p50/p95/p99
recheck throughput
peer serving throughput
tracker announce rate
event lag
DB size
crash recovery time
```

---

## 8. Security Model

### 8.1 API

- Bearer tokens for automation.
- Cookie + CSRF for browser UI.
- Localhost-only default bind.
- Optional Unix socket.
- Token scopes.
- Audit log for destructive actions.

Scopes:

```text
read
torrent:add
torrent:control
torrent:delete
torrent:move
settings:read
settings:write
admin
```

### 8.2 Filesystem

- All metadata paths sanitized.
- No absolute paths.
- No parent traversal.
- No symlink escape.
- No writes outside storage roots.
- No delete unless explicit `delete_files=true`.
- Cross-filesystem move uses copy-verify-commit.

### 8.3 Network

- Limit peer frame sizes.
- Limit tracker response sizes.
- Timeout all network operations.
- No DHT/PEX/LSD for private torrents by default.
- SSRF protections for URL-based torrent downloads.
- Optional outbound allowlist/denylist.

---

## 9. Implementation Phases

## Phase 0: Bootstrap

Deliver:

- workspace
- CI
- formatting/lints
- docs skeleton
- dependency policy
- test harness skeleton

Commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Acceptance:

- CI green
- no placeholder crates without README

## Phase 1: Metadata Core

Crates:

- rt-bencode
- rt-path
- rt-metainfo
- rt-piece-map

Acceptance:

- parse v1 torrents
- compute infohash correctly
- reject unsafe paths
- map pieces to files

## Phase 2: Storage + Verification

Crates:

- rt-storage
- rt-fastresume
- rt-jobs basics

Acceptance:

- import `.torrent` + existing files
- verify complete torrent
- detect corrupt pieces
- persist progress
- cancel/resume recheck

## Phase 3: Tracker Engine

Crate:

- rt-tracker

Acceptance:

- HTTP/UDP announce
- compact peers
- private tracker rules
- announce jitter
- stopped announce on shutdown

## Phase 4: Seeding Peer Engine

Crates:

- rt-peer-wire
- rt-peer-manager

Acceptance:

- seed complete torrent over TCP to test peer
- interop seed to qBit/Transmission
- upload accounting

## Phase 5: Daemon + DB

Crates:

- rt-db
- rt-session
- rusttorrentd

Acceptance:

- add/import torrent
- persist session
- restart
- expose health/metrics
- no global recheck on restart

## Phase 6: Native API

Crates:

- rt-api-model
- rt-api-native
- rt-sdk
- rtctl

Acceptance:

- list/add/pause/resume/recheck
- events stream
- jobs API
- token auth

## Phase 7: qBit API

Crate:

- rt-api-qbit

Acceptance:

- Prowlarr/Sonarr/Radarr add/list/control workflows
- qBit `sync/maindata`
- categories/tags subset

## Phase 8: Download Engine

Crate:

- rt-piece-picker

Acceptance:

- public torrent download via tracker
- verify pieces
- transition to seeding
- resume partial download

## Phase 9: Scale Certification

Acceptance:

- 15k torrents loaded
- 200+ TB simulated
- API p95 under target
- idle RSS under target
- no announce storm
- recheck bounded

## Phase 10: Web UI

Acceptance:

- virtualized torrent table
- server-side filter/sort
- jobs page
- diagnostics page
- storage page

## Phase 11: DHT/PEX/LSD/uTP

Acceptance:

- DHT public torrent works
- PEX works
- LSD optional
- uTP optional
- private mode keeps them off

## Phase 12: BEP52/v2/hybrid

Acceptance:

- parse v2/hybrid
- verify v2 piece layers
- seed/download v2

---

## 10. Agent Task Templates

### 10.1 Standard Implementation Prompt

```text
You are implementing one isolated crate in the RustTorrent workspace.

Before coding:
1. Read the crate README.
2. Read docs/agent-rules.md.
3. Read the acceptance criteria for this task.
4. Do not modify unrelated crates except shared models if necessary.
5. Do not add dependencies without updating docs/dependencies.md.

Task:
[INSERT TASK]

Required:
- Implement typed errors.
- Add unit tests.
- Add at least one negative test.
- Avoid unwrap/expect in library code.
- Run cargo fmt, clippy, and tests for touched crates.
- Update crate README if public API changed.

Stop and report if:
- protocol behavior is ambiguous
- existing tests conflict with requirements
- task requires unsafe Rust
- task requires global architecture change
```

### 10.2 Bug Fix Prompt

```text
You are fixing a bug in RustTorrent.

Rules:
1. First add a failing test that reproduces the bug.
2. Then fix the bug.
3. Do not broaden scope.
4. Do not refactor unrelated code.
5. Include a short root-cause note.

Bug:
[INSERT BUG]

Acceptance:
- New test fails before fix.
- New test passes after fix.
- Existing tests pass.
```

### 10.3 Protocol Implementation Prompt

```text
You are implementing a BitTorrent protocol component.

Rules:
1. Read the relevant BEP from docs/protocol/.
2. Implement only the specified subset.
3. Add parser tests for valid and invalid inputs.
4. Add fuzz target if parsing network input.
5. Add size limits for peer/tracker-controlled data.
6. Return typed errors, never panic.

Component:
[INSERT COMPONENT]

Acceptance:
[INSERT ACCEPTANCE]
```

---

## 11. First 25 Tickets

1. Create workspace skeleton and CI.
2. Add docs/agent-rules.md.
3. Implement `rt-bencode` parser.
4. Add bencode property tests.
5. Add bencode fuzz target.
6. Implement `rt-path` safe relative paths.
7. Implement `rt-metainfo` v1 parser.
8. Implement v1 infohash extraction from original bytes.
9. Implement file list normalization.
10. Implement `rt-piece-map`.
11. Add piece/file property tests.
12. Implement SQLite migration crate.
13. Implement storage root table.
14. Implement torrent metadata tables.
15. Implement verification job schema.
16. Implement bounded hashing worker.
17. Implement file verifier for complete files.
18. Implement resumable recheck progress.
19. Implement fake HTTP tracker.
20. Implement HTTP announce client.
21. Implement compact peer parser.
22. Implement private torrent policy object.
23. Implement peer wire handshake codec.
24. Implement peer message frame codec.
25. Implement daemon health endpoint.

---

## 12. Definition of "Do Not Worry"

This project is safe to hand to weaker models only if tasks are sliced as follows:

Good tasks:

- implement one parser
- add one endpoint
- add one DB migration
- add one typed model
- add one test fixture
- implement one state transition
- implement one codec message
- add one benchmark

Bad tasks:

- "implement BitTorrent"
- "make qBit compatibility work"
- "add storage engine"
- "optimize performance"
- "fix all bugs"
- "refactor session state"

Every task must have:

- exact files/crates
- exact acceptance criteria
- exact tests to add
- explicit non-goals
- allowed dependencies
- "stop if ambiguous" clause

---

## 13. Final Implementation Principle

The first useful release is not a general torrent client. It is:

> A private-tracker-safe, seeding-first Rust daemon that imports existing torrents, verifies files safely, announces correctly, serves pieces correctly, persists state, exposes qBit-compatible API, and remains responsive with 15k torrents.

Everything else is subordinate.
