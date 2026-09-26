# Next-agent hardening handoff

> Historical handoff: the concrete root-authority, egress-policy, and auth
> wiring items below were the starting checklist, not the current state. The
> current source disposition is maintained in
> [`docs/BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md), updated
> 2026-09-25. Check that ledger before reapplying any item.

## Current disposition (2026-09-25)

This handoff is retained as historical context, not as an active checklist.
The storage-root authority, egress policy, shared HTTP transport, peer ingress,
packed peer state, supervised persistence, snapshot/pagination, compatibility
honesty, and metrics-privacy items described below are implemented in the
current tree. This continuation also closed TNG-124 through TNG-129: backend
redirects are rejected; browser login/proxy-CSRF and legacy cookie fallback
are hardened; configured-token login is throttled; and peer-ingress failure
counters are exposed through `/metrics`. This continuation also closed
TNG-131 through TNG-134: failed batched database commands roll back partial
writes; native database-worker pressure/outcome/latency counters are exported;
the primary storage scheduler uses the RLIMIT-derived retained-cache ceiling;
checkpoint sync no longer opens every dirty file at once; and TNG-135 adds
lifecycle-tied active-descriptor admission to each path-backed scheduler
`FilePool`; TNG-136 applies a local lease cap to
`StorageRuntime::HandleCache`. TNG-138 makes those caches share one
process-level managed-storage budget; unrelated process descriptors remain
outside that quota. The Windows storage-plan executor now also opens configured
roots as capability directories and performs recursive operations and
no-replace rename relative to opened directory handles. Its library and test
targets cross-check for `x86_64-pc-windows-msvc`; native Windows runtime
qualification remains external.
Fresh evidence for those fixes is recorded in
[`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md).

Do not re-implement the old patch targets. External qualification gates remain
open as tracked in the canonical ledger; no public-network run, torrent-count
proof, or soak was performed in this continuation. If a new defect is found,
add a dated ledger entry with a reproducer rather than reopening historical
instructions by assumption.

## Historical branch context

This branch already contains several hardening primitives and safety fixes, but some of the highest-impact integrations require editing very large files (`crates/rt-engine/src/engine.rs`, `crates/rt-engine/src/torrent_task.rs`, and sometimes TorrentNG API handlers). The connector write API only supports whole-file replacement for those files. I intentionally did not reconstruct those giant files blindly without a local build/test loop.

Branch: `hardening/ruthless-review-fixes`

Current high-value additions already present:

- `crates/rt-engine/src/egress_policy.rs`
- `crates/rt-engine/src/peer_ingress.rs`
- `crates/rt-engine/src/storage_authority.rs`
- extended `CompactPieceBitmap` in `crates/rt-engine/src/tier.rs`
- config knobs for ingress and egress hardening in `crates/rt-config/src/lib.rs`
- metainfo numeric/cap hardening in `crates/rt-metainfo/src/parse.rs`
- first-party client daemon and compatible-client facade auth hardening in `crates/torrentngd/src/main.rs`, `crates/rt-api-native/src/router.rs`, and `crates/rt-api-qbit/*`

## Historical must-do before merge

Run:

```sh
cargo fmt
cargo test
cargo clippy --workspace --all-targets -- -D warnings
```

The branch is currently diverged from `main`. Rebase/merge current `main` before trusting the final diff.

## 1. Wire server-owned storage roots into storage execution

Problem:

`Engine::execute_storage_plan_job` currently accepts `roots: &[PathBuf]`. If `roots` is empty, it calls `rt_storage::execute_storage_plan_with_checkpoints`, which executes without root confinement. If `roots` is non-empty, they may be caller-provided. Execution authority must not come from the request.

Files:

- `crates/rt-engine/src/engine.rs`
- optionally `crates/rt-api-native/src/handlers.rs`

Already added helper:

- `crates/rt-engine/src/storage_authority.rs`
- exported as `ServerStorageRoots`

Patch target:

In `Engine::execute_storage_plan_job`, remove the `if roots.is_empty()` branch entirely for execution. Load configured storage roots from DB instead.

Suggested helper inside `impl Engine`:

```rust
fn configured_storage_roots_for_execution(&self) -> Result<Vec<PathBuf>, String> {
    let rows = {
        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::list_storage_roots(&db).map_err(|e| e.to_string())?
    };
    let paths = rows.into_iter().map(|row| PathBuf::from(row.path));
    ServerStorageRoots::from_configured_paths(paths)
        .map(ServerStorageRoots::into_roots)
        .map_err(|e| e.to_string())
}
```

Then in `execute_storage_plan_job`:

```rust
let server_roots = self.configured_storage_roots_for_execution()?;
let result = rt_storage::execute_storage_plan_under_roots_with_checkpoints(
    plan,
    &server_roots,
    &already_completed,
    checkpoint,
);
```

Recommended API change:

- Keep `roots` in preview request structs only.
- Remove or ignore caller roots in execute request path.
- If removing field would break API compatibility, leave it accepted but ignored and log/return a warning in the job event payload.

Tests:

- Execute with request roots including `/` must still reject paths outside configured roots.
- Execute with no configured roots must fail closed.
- Preview can still evaluate caller-provided roots for planning.

## 2. Wire outbound egress policy into trackers and webseeds

Problem:

Tracker/webseed URLs are attacker-controlled. The new egress policy module exists but is not yet enforced in the live tracker/webseed code paths.

Files:

- `crates/rt-engine/src/torrent_task.rs`
- `crates/rt-engine/src/egress_policy.rs`
- `crates/rt-config/src/lib.rs`

Already added:

- `OutboundEgressPolicy`
- `OutboundTargetKind`
- config mapping via `From<&rt_config::TrackerConfig>`
- tracker config fields:
  - `allow_http_trackers`
  - `allow_https_trackers`
  - `allow_udp_trackers`
  - `allow_http_webseeds`
  - `allow_https_webseeds`
  - `allow_loopback_egress`
  - `allow_private_egress`
  - `allow_link_local_egress`
  - `allow_multicast_egress`
  - `allow_unspecified_egress`

Patch target:

Add field to `TorrentTask`:

```rust
egress_policy: OutboundEgressPolicy,
```

Add constructor parameter from engine:

```rust
OutboundEgressPolicy::from(&self.config.tracker)
```

In `announce_http`:

```rust
let parsed = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
self.egress_policy
    .validate_url(OutboundTargetKind::Tracker, &parsed)
    .map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
```

After DNS resolution for UDP trackers, before connect:

```rust
self.egress_policy
    .validate_socket_addr(tracker_addr)
    .map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
```

For HTTP trackers and webseeds, reqwest hides DNS resolution. To enforce post-resolution policy properly, either:

1. pre-resolve host with `tokio::net::lookup_host`, validate all resolved IPs, and only then issue reqwest request; or
2. implement a custom reqwest resolver layer later.

Use option 1 initially: validate resolved addresses before sending request. It is not perfect against DNS changes between lookup and connect, but it closes the obvious loopback/private/meta-service abuse until a custom resolver is added.

Webseed fetch path should use `OutboundTargetKind::Webseed` and the same DNS/IP checks.

Tests:

- HTTP tracker to `127.0.0.1` rejected by default.
- UDP tracker to `192.168.0.1` rejected by default.
- Webseed `file://` or `udp://` rejected.
- Private LAN allowed only when config says so.

## 3. Reuse HTTP clients for tracker announce/scrape/webseed

Problem:

`TorrentTask` already has a `webseed_client`, but `announce_http` and `scrape_tracker` still build new `reqwest::Client`s in the hot path. That loses pooling and wastes allocations.

File:

- `crates/rt-engine/src/torrent_task.rs`

Patch target:

Rename field:

```rust
http_client: reqwest::Client,
```

Build once in `TorrentTask::new`:

```rust
let http_client = reqwest::Client::builder()
    .timeout(Duration::from_secs(http_timeout_secs.max(1)))
    .user_agent(crate::peer_id::USER_AGENT)
    .pool_max_idle_per_host(8)
    .tcp_keepalive(Some(Duration::from_secs(60)))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());
```

Use it in:

- `announce_http`
- `scrape_tracker`
- webseed block download path

Replace:

```rust
reqwest::Client::builder()...
reqwest::Client::new()
```

with:

```rust
self.http_client.get(url) ...
```

Also add response body caps before parsing tracker responses.

## 4. Wire peer ingress budget into engine accept loop

Problem:

Per-torrent `max_peers` does not protect the daemon from public accepted-but-not-routed peer storms. The branch now has a tested `PeerIngressBudget`, but it is not wired into `Engine::run` accept paths.

Files:

- `crates/rt-engine/src/engine.rs`
- `crates/rt-engine/src/peer_ingress.rs`
- `crates/rt-config/src/lib.rs`

Already added:

- `PeerIngressBudget`
- `PeerIngressConfig`
- config fields for global/per-IP/timeout knobs.

Patch target:

Add field to `Engine`:

```rust
peer_ingress: Arc<PeerIngressBudget>,
```

Initialize in `Engine::start`:

```rust
let peer_ingress = Arc::new(PeerIngressBudget::new(PeerIngressConfig {
    max_global_handshakes: config.network.max_incoming_handshakes,
    max_handshakes_per_ip: config.network.max_incoming_handshakes_per_ip,
    per_ip_window: Duration::from_secs(config.network.incoming_handshake_window_secs),
    handshake_timeout: Duration::from_secs(config.network.incoming_handshake_timeout_secs),
}));
```

In TCP accept branch:

```rust
let ingress = self.peer_ingress.clone();
...
match ingress.try_begin(peer_addr, Instant::now()) {
    Ok(permit) => tokio::spawn(async move {
        let _permit = permit;
        let result = timeout(ingress.config().handshake_timeout, handle_incoming(stream, peer_addr, chans)).await;
        ...
    }),
    Err(err) => warn!(...)
}
```

Do same for uTP accept path.

Metrics (completed locally):

`EngineHandle::peer_ingress_stats()` now exposes the listener's shared
`PeerIngressStats`, and TorrentNG-client `/metrics` exports admitted
handshakes, global/per-IP/global-peer-budget rejections, handshake read errors,
timeouts, and malformed protocol handshakes. Loopback unit tests exercise the
timeout and malformed paths plus the Prometheus renderer. This is local
instrumentation coverage, not public-network qualification evidence.

Optimization:

Avoid cloning the entire `torrent_chans` `HashMap` per accepted connection in the long term. This can stay for this patch if needed, but the real scale fix is an `Arc<RwLock<HashMap<...>>>` routing table or compact immutable route snapshot.

## 5. Replace `peer_has: Vec<bool>` with compact bitmap

Problem:

`PeerHandle` stores `peer_has: Vec<bool>`. For huge torrents and many peers this becomes expensive and poorly accounted.

File:

- `crates/rt-engine/src/torrent_task.rs`

Already improved:

- `CompactPieceBitmap` in `crates/rt-engine/src/tier.rs` now supports:
  - `missing`
  - `complete`
  - `set_piece`
  - `set_all_from_bitfield`
  - `to_bitfield`
  - `complete_pieces`
  - `estimated_heap_bytes`

Patch target:

Change:

```rust
peer_has: Vec<bool>,
```

To:

```rust
peer_has: CompactPieceBitmap,
```

At registration:

```rust
peer_has: CompactPieceBitmap::missing(self.meta.pieces.len() as u32),
```

On bitfield event:

```rust
peer.peer_has.set_all_from_bitfield(&pieces)?;
```

On have event:

```rust
peer.peer_has.set_piece(piece, true);
```

Where existing code calls `pieces_to_bitfield(&peer.peer_has)`, either:

- use `peer.peer_has.to_bitfield()` as compatibility glue; or
- update `Availability` to accept compact bytes directly later.

Stats:

Replace `peer.peer_has.capacity()` accounting with `peer.peer_has.estimated_heap_bytes()`.

## 6. DB actor / blocking DB worker

Problem:

SQLite is currently behind `Arc<Mutex<Connection>>` and used from async engine/torrent paths. This is acceptable for early scaffolding but not best-in-class under sustained load.

Files:

- `crates/rt-engine/src/engine.rs`
- `crates/rt-engine/src/torrent_task.rs`
- `crates/rt-db/*`

Recommended phased patch:

1. Introduce `rt_engine::db_actor` with bounded channel.
2. DB actor owns `rusqlite::Connection` on a dedicated blocking thread.
3. Engine/torrent tasks send typed commands.
4. Add metrics: queue depth, command latency, transaction latency, failures.
5. Convert hot paths first: progress persistence, tracker state persistence, session events, job updates.
6. Convert read APIs second.

Do not attempt this as a tiny patch. It is a real architecture step.

## 7. Topology-aware storage scheduler defaults

Problem:

Storage scheduler primitives are good, but defaults are static. Large deployments may benefit from topology-aware scaling. The shared process-level managed-storage descriptor budget is implemented and tracked as TNG-138 in the canonical burn-down.

Files:

- `crates/rt-storage/src/scheduler.rs`
- `crates/rt-storage/src/device.rs`
- `crates/rt-config/src/lib.rs`

Recommended patch:

- Add `StorageAutoTuningConfig` or derive in `storage_io_config_from_config`.
- For HDD/network mounts: low concurrency, larger elevator budget, careful read batching.
- For SSD/NVMe: higher queue depth, more workers, less elevator delay.
- For CoW filesystems: sparse preallocation default, avoid full preallocation unless explicitly requested.

Required tests:

- HDD profile picks conservative concurrency.
- NVMe profile picks larger queue depth.
- CoW profile avoids full preallocation in auto mode.
- The managed-storage FD budget's cross-cache admission, wakeup, and metrics regressions are recorded under TNG-138.

## 8. Historical branch caveats (superseded)

The bullets below describe the old handoff branch, not the current checkout:
tests were not run there, the branch was reported diverged, and enforcement
wiring was still listed as pending. The current implementation and fresh
verification are tracked in `BACKEND_AUDIT_BURN_DOWN.md`; do not use those old
caveats as a reason to repeat the integration work.

## Historical suggested next-agent order (superseded)

The sequencing below is retained as history only; the listed authority, egress,
transport, ingress, and packed-peer-state wiring is already present. The DB
worker is also implemented. Treat topology-aware scheduler tuning as a separate
future optimization, not as an unfinished item from this handoff. Check the
canonical ledger for current local defects and external gates.
