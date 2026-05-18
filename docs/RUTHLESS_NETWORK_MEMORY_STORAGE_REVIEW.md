# Ruthless network / memory / storage review

This review is intentionally hostile. The target is not "does it work on a happy path". The target is: can TorrentNG survive hostile torrents, hostile peers, high-latency storage, huge libraries, bad operator config, reverse proxies, and sustained high-throughput workloads without lying to the user or corrupting data.

Status: not yet. There are strong foundations, but several hot paths and trust boundaries are not yet robust enough for a best-in-class torrent daemon.

## Executive verdict

TorrentNG is on the right architectural track: per-torrent tasks, bounded channels, storage scheduling, resource-governor accounting, tracker state persistence, DHT/private-torrent suppression hooks, staged storage plans, and useful metrics already exist.

The weak points are exactly where high-performance torrent clients fail in production:

- metainfo parser hardening is incomplete;
- tracker and webseed outbound policy is too permissive;
- tracker HTTP clients are constructed in hot paths;
- peer ingress lacks a visible global unauthenticated-handshake budget;
- storage execution authority still needs to be server-owned end to end;
- database calls are synchronous behind a mutex inside async engine code;
- default storage/memory settings are plausible but not topology-scaled;
- compatibility facades still have too much accepted/inert behavior;
- metrics are extensive but need sharper release-gate interpretation.

## P0: metainfo parser hardening

Files inspected:

- `crates/rt-metainfo/src/parse.rs`
- `crates/rt-metainfo/src/error.rs`

Finding: torrent-controlled signed integers are cast to `u64` in several places. Examples include `piece length`, v1 file `length`, v2 file-tree `length`, and offsets accumulated from those values. Negative values can wrap into huge unsigned values. This is a direct memory/storage correctness bug.

Current branch change:

- Added metainfo error variants in `error.rs`: `InvalidIntegerValue`, `IntegerOverflow`, and `LimitExceeded`.

Required code patch:

- Replace every `get_int(...)? as u64` with checked helpers.
- Add `get_nonnegative_u64(dict, key, field)`.
- Add `get_positive_power_of_two_u64(dict, key, field)` for piece length.
- Use `checked_add` when advancing file offsets.
- Cap raw `.torrent` size, file count, path component count, tracker count, webseed count, and piece count.

Recommended helper shape:

```rust
fn get_nonnegative_u64(dict: &BValue<'_>, key: &[u8], field: &'static str) -> Result<u64, MetainfoError> {
    let value = get_int(dict, key, field)?;
    u64::try_from(value).map_err(|_| MetainfoError::InvalidIntegerValue { field, value })
}

fn add_offset(offset: &mut u64, length: u64, field: &'static str) -> Result<(), MetainfoError> {
    *offset = offset.checked_add(length).ok_or(MetainfoError::IntegerOverflow(field))?;
    Ok(())
}
```

Required tests:

- v1 single-file `length = -1` rejected.
- v1 multi-file `length = -1` rejected.
- v2 file-tree `length = -1` rejected.
- `piece length = -1` rejected.
- `piece length = i64::MIN` rejected.
- piece vector count cap enforced.
- file count cap enforced.
- tracker/webseed count cap enforced.

## P0: storage execution authority

Files inspected:

- `crates/rt-api-native/src/handlers.rs`
- `crates/rt-storage/src/plan.rs`
- `crates/rt-engine/src/engine.rs`

Finding: the storage planner itself is careful. It rejects symlink entries, validates root confinement, stages copy/rename flows, verifies sizes, avoids destination overwrite, and has rollback tests. That is good.

The weak point is authority: `/api/v1/storage/execute` still accepts `roots` in the request and passes them to execution. Client-supplied roots are fine for preview simulation but not for execution. Execution roots must come from server configuration or persisted storage-root rows only.

Required code patch:

- `storage_preview_plan`: may accept request roots for simulation.
- `storage_execute_plan`: must ignore request roots and ask the engine for configured roots or pass no roots and let the engine resolve them.
- `EngineCmd::ExecuteStoragePlan`: should not receive caller roots, or it should treat them as advisory only.
- `execute_storage_plan_job`: should load roots from `rt_db::list_storage_roots` and canonicalize them.
- Add a clear failure when no configured writable root exists.

Required tests:

- Preview can reject outside-root paths with caller-provided root list.
- Execute rejects outside-root paths even if caller provides `/` as a root.
- Execute rejects symlink source/destination escapes.
- Execute allows only configured roots persisted by `register_configured_storage` or future admin root registration.

## P0: outbound tracker and webseed policy

Files inspected:

- `crates/rt-engine/src/torrent_task.rs`
- `crates/rt-metainfo/src/parse.rs`

Finding: tracker URLs and webseed URLs are accepted as strings and later used for outbound traffic. There is scheme checking in announce paths, but no server-owned egress policy layer.

Required policy:

- Allowed tracker schemes: `udp`, `http`, `https` by explicit config.
- Allowed webseed schemes: `http`, `https` by explicit config.
- Optional block policy for loopback, link-local, private LAN, multicast, unspecified, documentation ranges, and operator-specified suffixes/domains.
- DNS resolution policy should be enforced after lookup, not only by hostname string.
- Private torrents must default to no DHT, no PEX, no LSD, and no accepting peers not returned by tracker/manual allowlist.

Current good sign:

- Private torrent inbound filtering exists via `allowed_private_peers` and `peer_source_allowed` style logic.
- DHT registration path checks private metadata before registering torrents.

Remaining concern:

- Magnets and metadata-pending paths must be audited so private-flag discovery retroactively suppresses DHT/PEX and clears leaked peer sources.

## P1: tracker HTTP client construction is inefficient

File inspected:

- `crates/rt-engine/src/torrent_task.rs`

Finding: `TorrentTask` already owns a `webseed_client`, but HTTP tracker announce and scrape construct new `reqwest::Client` instances inside announce/scrape methods. That is unnecessary per-announce overhead and loses connection pooling.

Required patch:

- Rename `webseed_client` to something like `http_client` or add a second `tracker_client`.
- Build it once in `TorrentTask::new` with timeout, user-agent, redirect policy, TCP keepalive, and pool settings.
- Use it in `announce_http`, `scrape_tracker`, and webseed fetching.
- Cap tracker response body size before parsing.

Expected benefit:

- Less allocation and TLS/client setup churn.
- Better connection reuse for trackers and webseeds.
- More predictable latency under many active torrents.

## P1: peer ingress needs a global pre-handshake budget

Files inspected:

- `crates/rt-engine/src/engine.rs`
- `crates/rt-engine/src/torrent_task.rs`

Finding: incoming TCP/uTP peers are accepted by the engine and handed off after handshake routing. Per-torrent `max_peers` exists, and torrent tasks reject once active peers are full. That is not enough for hostile public ingress.

Required behavior:

- Global semaphore for accepted-but-not-yet-routed incoming TCP peers.
- Separate semaphore for accepted-but-not-yet-routed uTP peers.
- Handshake timeout for every incoming transport.
- Per-IP short-window rejection counter.
- Metrics for inbound accepted, routed, rejected full, rejected malformed, timed out, private-policy rejected.
- Avoid cloning the entire torrent-channel map for every accepted socket if torrent count grows large.

Current concern:

- The accept path clones `torrent_chans` for handoff. With thousands of torrents, this can become a serious per-connection cost. Prefer `Arc<RwLock<HashMap<...>>>`, a routing service, or a compact immutable routing snapshot with cheap clone semantics.

## P1: torrent task memory and channel pressure

File inspected:

- `crates/rt-engine/src/torrent_task.rs`

Good:

- Piece assembly has explicit soft caps.
- Peer request pipeline shrinks under assembly pressure.
- Tracker peer cache is bounded.
- Peer command channels are bounded.
- Runtime stats expose queue depth and memory estimates.

Concerns:

- `peer_event_tx` channel is hardcoded at 512 per torrent. For many active torrents, this is a hidden memory multiplier. Make it configurable or derived from peer limits.
- `register_peer` creates `peer_has: vec![false; piece_count]` per peer. For huge torrents with many pieces, this is expensive. Prefer a compact bitmap representation.
- `PeerHandle.requested` uses a Vec of block requests. At high peer counts and deep pipelines, the memory model should include exact request-list capacity.
- `runtime_stats` estimates `peer_command_queue_bytes` with `peer.peer_has.capacity()` but not `size_of::<bool>()` semantics or request vector capacity. The accounting is directionally useful but not strict enough for hard resource enforcement.

Required patch direction:

- Replace peer piece availability Vec<bool> with bitset/bitmap storage.
- Add per-torrent configurable event channel capacity.
- Charge peer availability maps and outstanding request storage to `MemoryClass::PeerBuffer` or a new peer-state class.
- Make request pipeline adapt to total ResourceGovernor pressure, not only piece assembly bytes.

## P1: database actor instead of sync mutex inside async engine

Files inspected:

- `crates/rt-engine/src/engine.rs`
- `crates/rt-engine/src/torrent_task.rs`

Finding: SQLite is behind `Arc<Mutex<Connection>>`. This can work, but it is fragile in a Tokio service if a path holds the mutex while doing slow work or if persistence frequency rises under load.

Required patch direction:

- Introduce a DB actor or blocking DB worker thread.
- Engine and torrent tasks send DB commands over bounded channels.
- DB actor owns connection and batching policy.
- Add metrics for DB queue depth, write latency, transaction duration, and checkpoint duration.
- Persist progress with coalescing and backpressure behavior instead of direct lock pressure.

## P1: storage scheduler is promising but needs topology-aware defaults

Files inspected:

- `crates/rt-storage/src/scheduler.rs`
- `crates/rt-storage/src/plan.rs`
- `crates/rt-config/src/lib.rs`

Good:

- Per-mount scheduler abstraction exists.
- Storage latency buckets exist.
- Queue-full metrics exist.
- File pool exists.
- Peer-read cache/elevator exists.
- Page-cache advice metrics exist.
- Sparse extent accounting exists.
- Resource-governed queued disk bytes exist.

Concerns:

- Default worker counts and queue depths are static: 4 I/O workers, 2 hash workers, 256 queue depth. That may be too small for NVMe and too aggressive for HDD arrays depending on topology.
- Config validates almost nothing: invalid pressure thresholds, zero/huge worker values, weird memory class budgets, huge file pool sizes, and pathological queue depths can get through.
- File pool capacity is global per scheduler, but huge multi-file torrents and many active torrents need better aggregate FD budgeting.
- `BlockingPool` uses a synchronous channel plus worker threads. That is acceptable, but tuning must be exposed and measured as a first-class performance surface.

Required patch direction:

- Add config validation and clamping with warnings.
- Add topology-derived defaults: HDD, SSD, NVMe, network mount, CoW filesystem.
- Add global process FD budget and per-scheduler leasing.
- Surface storage profile in `/api/v1/storage` and metrics.
- Add soak tests for HDD-like seek storms, NVMe parallel reads, and mixed recheck/download/upload workloads.

## P1: webseed loop can become a polling tax

File inspected:

- `crates/rt-engine/src/torrent_task.rs`

Finding: every torrent runs a `webseed_tick` every 100ms and then frequently returns early. With thousands of torrents, periodic idle ticks become overhead.

Required patch direction:

- Disable webseed interval when no webseeds exist.
- Use adaptive scheduling: only arm webseed work when no peers and pieces remain.
- Back off after repeated webseed failures.
- Prefer a per-torrent next-deadline mechanism over fixed high-frequency tick.

## P2: metrics are extensive but need SLO-level summaries

Files inspected:

- `crates/rt-api-native/src/handlers.rs`
- `crates/rt-storage/src/scheduler.rs`
- `crates/rt-metrics/src/resource.rs`

Good:

- Metrics coverage is unusually broad for early code.
- Storage latency histograms exist.
- Memory class snapshots exist.
- Queue-full and denied-allocation counters exist.

Missing:

- A small set of top-level health/SLO metrics: scheduler saturation, DB saturation, peer ingress rejection, tracker error ratio, active bottleneck class, storage p95/p99, and memory pressure state.
- Operator guidance for what thresholds are bad.
- A benchmark/soak profile that asserts no regression for 1k, 5k, 15k torrent libraries.

## Throughput/performance target architecture

Best-in-class direction:

1. One long-lived HTTP client per torrent task or shared per daemon with policy-injected request metadata.
2. Global peer ingress accept budget, per-IP throttles, and handshake timeout.
3. Per-torrent peer state stored compactly; no giant per-peer Vec<bool> for availability.
4. Storage scheduler defaults derived from topology, not static constants.
5. DB actor with bounded queue and explicit backpressure.
6. Strict metainfo parser caps and outbound policy.
7. Release-gated performance profiles:
   - 15k torrents loaded, zero active.
   - 15k torrents loaded, 500 active announces.
   - 100 active downloads on NVMe.
   - 100 active seeds on HDD/media pool.
   - recheck while seeding.
   - hostile peer connect storm.
   - malformed torrent corpus.

## Bottom line

TorrentNG is not yet as robust or efficient as it can be. It has the skeleton of a serious high-throughput daemon, but the next phase must be less feature-surface and more defensive systems engineering:

- stop hostile inputs at parser and egress boundaries;
- make every queue, worker, file descriptor, and memory budget explicit;
- reuse expensive clients/resources;
- make storage and DB backpressure first-class;
- prove performance with repeatable soak profiles.
