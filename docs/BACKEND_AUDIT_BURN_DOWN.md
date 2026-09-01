# TorrentNG Backend Audit Burn-down

Status: **active**  
Baseline: 2026-09-01, `main`  
Scope: native Rust engine, native daemon/API, compatibility facades, storage,
deployment, CI, and release evidence.

This is the canonical remediation ledger for the principal-engineer / investor
audit. Older roadmap and certification documents describe intended or locally
tested behavior; they are not proof that a feature is wired into the live
runtime. An item is not complete until its code path, focused regression test,
and release evidence exist together.

## Executive decision

TorrentNG is not currently credible as a production-grade 100k-torrent engine
or as a universally compatible client. The low-level storage, parser, and
protocol crates have useful foundations, but the orchestration and trust
boundaries are not finished. The current release posture is **do not make
unqualified scale, security, pure-v2, or universal-compatibility claims**.

The burn-down order is:

1. security and data integrity;
2. runtime limits, lifecycle, and scale;
3. API and compatibility truth;
4. deployment, CI, and independent release evidence;
5. architecture seams and maintainability.

## Status rules

- **Open** — finding reproduced or independently evidenced; no complete fix.
- **In progress** — code or documentation work exists, but acceptance criteria
  are not all met.
- **Blocked** — external hardware/client/operator evidence is required and the
  local repository cannot produce it.
- **Resolved** — implementation, regression coverage, and required evidence are
  present. A passing unit test alone is not enough.

Severity is an engineering priority, not a statement about exploitability in a
particular private deployment. P0 means release-blocking for any deployment
that exposes the affected surface. P1 means material production risk. P2 means
important correctness, evidence, or maintainability debt.

## Baseline evidence

The following was run against the audit baseline before this burn-down began:

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | Existing native tests are green, but mostly exercise isolated behavior. |
| `cargo test --manifest-path sidecar/Cargo.toml --locked` | PASS | Sidecar tests are green. |
| `cargo fmt --all -- --check` | FAIL | `crates/rt-migrate/src/lib.rs:2663` was not formatted. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | FAIL | Existing lint/MSRV/enum-layout failures remain. |
| native CI workflow | INCOMPLETE | `.github/workflows/ci.yml` builds sidecar/WebUI but does not test native crates. |
| native release workflow | INCOMPLETE | Release builds and smoke-checks the binary without native test, fmt, or clippy gates. |
| certification status | NOT CLEAN | Universal compatibility is `PASS_WITH_SKIPS`; 24h soak is stale/incomplete; strict readiness fails. |
| checked-in fuzz/OpenAPI/idempotency evidence | ABSENT | Documentation claims are not backed by checked-in targets or gates. |

## Current verified evidence (2026-09-01, second session)

A prior remediation pass (same date) left the tree with real progress but a
red baseline: `cargo test --workspace --all-targets --locked` hung
indefinitely, and two of the tests that could run were failing. This session
resumed from that point. Everything below was re-run and passed after fixes
recorded in the burn-down log:

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | Was hanging (see log: `network_budget` livelock). All crates green. |
| `cargo test --manifest-path sidecar/Cargo.toml --locked` | PASS | Unaffected by native-engine changes. |
| `cargo fmt --all -- --check` | PASS | Was failing (pre-existing `rt-migrate` issue plus new unformatted test code); now clean. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | Two findings fixed (see log below). |
| `cargo +1.88 build/test --workspace --all-targets --locked` | PASS | See MSRV correction below. |
| `cargo +1.97 build/test --manifest-path sidecar/Cargo.toml --locked` | PASS | See MSRV correction below. |
| declared `rust-version` (both `Cargo.toml`s) | **CORRECTED** | Was `1.80` in both, unverified and untrue. Neither workspace's *locked* dependency graph builds below 1.88 (main: `idna_adapter` needs rustc 1.86+, plus `edition2024` needs Cargo 1.85+) or 1.97 (sidecar: `libsqlite3-sys`'s build script uses `cfg_select!`, stabilized between 1.94 and 1.97). This is a transitive-dependency floor, not first-party code needing new syntax. Corrected both `rust-version` fields to `1.88` / `1.97` to match reality; this itself is TNG-028 acceptance criteria ("document the supported toolchain"). |

CI still does not run any of this (TNG-025, still Open) -- these are local,
manually-run results, not an enforced gate yet.

## P0 — security and data integrity

### TNG-001 — Server-owned storage authority is bypassable

**Status: In progress** · **Priority: P0** · **Confidence: high**

Verified evidence -- this is further along than a first pass of the diff
suggested; re-checked properly rather than left as Open by default. New
`ServerStorageRoots::authorize_path()`: rejects non-absolute paths and any
`..` component outright, then walks up to the nearest *existing* ancestor,
canonicalizes it (resolving symlinks), and requires that canonical
ancestor to be at or under a configured root -- closing the plain
absolute-path-escape and symlink-at-admission cases named in the finding.
6 unit tests cover it. `Engine::authorize_storage_path()` wraps it against
the live configured roots and has **12 real call sites** across add,
magnet-add, task restore/startup, save-path update, storage-plan
execution, and move source/destination -- this is broad, not a token
gesture. Deletion specifically was checked and is now covered:
`delete_payload_files()` calls `authorize_storage_path` on both the
save_path root and *every individual resolved file path* before removing
it, directly closing the finding's explicit "deletion trusts the stored
path" complaint.

The module's own doc comment is honest about what's left: "the actual file
operation still needs race-resistant, descriptor-relative enforcement
before this can be treated as a complete sandbox." That is the real
remaining gap -- this is admission-time path validation with a
canonicalize-then-check pattern, which is inherently TOCTOU-able (the
filesystem can change between the canonicalize check and the actual
`std::fs::remove_file`/open call). Not done: descriptor-relative
operations (`openat`-style, anchored to an already-opened root fd) or an
equivalent race-closing primitive: preview-roots-are-simulation-only
enforcement; fail-closed-with-no-writable-root behavior (exists as
`NoConfiguredRoots` but not independently verified here); and the full
acceptance suite (outside-root, symlink, broken-symlink, missing-ancestor,
restart, concurrent-plan tests).

Evidence: normal add/magnet paths accept caller-selected `save_path` in
`crates/rt-engine/src/engine.rs` and construct a scheduler directly in
`crates/rt-engine/src/torrent_task.rs`. Live file opens in
`crates/rt-storage/src/scheduler.rs` use ordinary path-based `OpenOptions`.
Storage-plan execution also accepts caller-provided roots even though the
newer plan path can reload persisted roots. Deletion trusts the stored path.

Required action:

- make server-configured/persisted roots the only authority for all execution,
  including add, magnet, move, delete, recheck, and task startup;
- reject absolute paths outside an authorized root and reject unsafe relative
  paths;
- defend every filesystem operation against symlink and ancestor races, using
  descriptor-relative operations or an equivalent platform-specific primitive;
- keep preview roots explicitly simulation-only;
- fail closed when no writable root is configured.

Acceptance: outside-root, symlink, broken-symlink, missing-ancestor, restart,
delete, and concurrent-plan tests; execution ignores caller roots; Linux
descriptor-relative integration coverage where supported.

### TNG-002 — Storage moves can race active writes

**Status: In progress** · **Priority: P0** · **Confidence: high**

Original evidence: `update_torrent_fields_inner` could start a move while the
torrent task continued writing. Cached handles and task-local storage state
remained live while the path changed -- `TorrentTask` caches its `save_root`
field at spawn time and never updated it, so a running task kept
reading/writing the *old* path forever after any move, even though the DB's
own `save_path` had already changed.

Verified evidence (this session): a real quiesce/resume protocol now exists
and is wired into both places a move or other in-place storage operation can
happen.

- Two new `TorrentCmd` variants (`crates/rt-engine/src/torrent_task.rs`):
  `QuiesceForStorageMove { reply: oneshot::Sender<bool> }` disconnects every
  peer, drains any peer event already buffered in the channel before
  replying (so a leftover `Block` event from just before disconnect can't
  still reach `handle_block` after the reply fires), and replies with
  whether the torrent was already paused beforehand. `ResumeAfterStorageMove
  { new_save_root: Option<PathBuf>, resume_paused: bool }` re-points
  `save_root` and rebuilds the `MountScheduler` bound to it (re-running
  device-topology detection rather than staying pinned to the pre-move
  mount's profile) when a move committed, clears `prepared_files` (stale
  bookkeeping from the old location), and resumes activity -- including
  re-running a recheck -- unless the torrent was paused before the move
  began. Both commands are also handled inside `pending_recheck_control`
  (an in-progress recheck is itself a reader that must stop before a move
  touches its files) and in `metadata_task.rs` (a no-op reply for
  not-yet-materialized torrents, since there are no files yet to race).
- `engine.rs`'s `move_torrent_payload_files` (the real save_path-changing
  path, reached from `update_torrent_fields_inner`) now quiesces the
  torrent's running task (if any) before calling `execute_storage_plan_job`,
  and resumes it afterward with `new_save_root: Some(destination)` on
  success or `None` on failure -- so a failed/rolled-back move leaves the
  task's cached path untouched.
- The generic `EngineCmd::ExecuteStoragePlan` handler (backing
  `POST /api/v1/storage/execute`, which operates on raw filesystem paths
  and never itself changes a torrent's persisted `save_path`) now quiesces
  every torrent listed in `affected_torrents` before executing the plan and
  resumes them all afterward unchanged (`new_save_root: None`) -- this was
  the "API storage-plan execution" gap the original finding explicitly
  called out.
- New test `update_save_path_reroutes_running_task_and_recheck_finds_new_root`
  (`crates/rt-engine/src/engine.rs`) proves this against a *real* spawned
  `TorrentTask`, not just the taskless path: a genuinely running task's
  torrent is moved while active, and a real, correctly SHA-1-hashed payload
  is placed only at the destination -- the post-move recheck the resume
  protocol triggers must find it there and reach `Seeding`. Verified this
  is a real regression test, not a tautology: temporarily reverted the
  `save_root` reassignment (kept the `MountScheduler` rebuild) and confirmed
  the test fails with `Downloading` (piece reported missing, read from the
  stale path) before restoring the real fix.
- Side finding while building that test, fixed as part of this work:
  `rt-session`'s `TorrentEntry::transition` table (`crates/rt-session/src/torrent.rs`)
  had no `(Seeding, Checking)` or `(Seeding, Downloading)` entries. Since
  `set_state` discards `transition()`'s `Result` (`let _ = entry.transition(state)`),
  rechecking an already-seeding torrent -- via the pre-existing
  `TorrentCmd::Recheck` command, not just this session's new code -- could
  never have its outcome reflected in the registry: the state field stayed
  stuck on stale `Seeding` no matter what the recheck actually found. Added
  the two transitions plus regression test `seeding_torrent_can_be_rechecked`.
- Full workspace `cargo test --workspace --all-targets --locked`,
  `cargo fmt --all -- --check`, and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` all green
  (`rt-engine` 127 tests, up from 126; `rt-session` 19, up from 18).
  `cargo test --manifest-path sidecar/Cargo.toml --locked` unaffected (75
  passed).

Not yet evidenced (why this stays "In progress", not "Resolved"): the
acceptance list's move-under-upload/move-under-download tests with a real
*active peer connection* transferring blocks during the move are not
covered -- the new test proves the task's cached path is correctly
re-pointed and a post-move recheck is correctly triggered, but doesn't
drive it through a live peer-wire handshake concurrently with the move
(no such loopback-peer test harness exists yet in this crate to reuse).
Cancellation, crash-mid-move, and restart-after-interrupted-move tests are
also still missing -- `execute_storage_plan_with_checkpoints` already
supports resuming from a partial `completed_steps` list (pre-existing), but
nothing exercises quiesce/resume specifically across a daemon restart.
`TorrentCmd::Shutdown` itself is not yet integrated with this protocol (a
shutdown racing an in-flight move is a distinct, still-open scenario).

Required action (remaining): a live-peer move-under-transfer test (needs a
loopback peer-wire harness); cancellation and crash/restart tests around an
in-flight move; decide whether `Shutdown` should itself quiesce/wait on any
in-flight storage-plan execution rather than racing it.

Acceptance: move-under-download (missing), move-under-upload (missing),
cancellation (missing), crash (missing), rollback (done -- see TNG-003's
rollback-failure-surfacing work, which this protocol also benefits from),
and restart (missing) tests with no writes to the old path after commit
(done for the quiesce/resume window itself).

### TNG-003 — Copy verification and rollback semantics are too weak

**Status: In progress** · **Priority: P0** · **Confidence: high**

Original evidence: `crates/rt-storage/src/plan.rs` verified aggregate lengths
rather than content hashes, and rollback dropped failed steps instead of
reporting a complete and independently auditable rollback result.

Verified evidence (this session): both headline complaints are now fixed with
real code and real tests, not just claims.

- Content verification: `copy_verify()` (`plan.rs:609`) now calls
  `verify_content_matches()` (`plan.rs:641`) after `verify_path_len()`
  succeeds -- a streaming SHA-1 comparison (`hash_file_sha1`, 64KB buffer,
  never loads a whole file into memory) of every regular file, recursing
  through directories, rejecting symlinks on either side, and erroring
  clearly on a dir/file type mismatch. New test
  `verify_content_matches_detects_bit_flip_despite_matching_length` proves
  this directly: two 10-byte files, one byte different, same length --
  `verify_path_len` alone would have accepted it, `verify_content_matches`
  correctly rejects it with a `StagedMoveFailed { step: "verify-content" }`
  and a "content hash mismatch" message.
- Rollback honesty: `StoragePlanExecution` gained a
  `rollback_failures: Vec<(StoragePlanStep, String)>` field and a
  `rollback_fully_succeeded()` helper. `rollback_plan()` (`plan.rs:568`) was
  rewritten to return `(Vec<StoragePlanStep>, Vec<(StoragePlanStep, String)>)`
  -- it used to silently drop any rollback step that itself failed
  (`if execute_step(step).is_ok() { ... }`, discarding the `Err` entirely).
  Now every rollback step is still attempted (a failing one does not abort
  the rest), and failures are captured with their reasons. Since the only
  caller that currently exists (`engine.rs`'s `execute_storage_plan_job`)
  only persists `error.to_string()` and discards the returned
  `StoragePlanExecution`, the failure detail is folded into the returned
  `StorageError::StagedMoveFailed` message itself
  (`execute_storage_plan_with_checkpoints`, `plan.rs:290`), e.g. "...;
  ADDITIONALLY 1 rollback step(s) failed and left the filesystem in a
  partial state requiring manual attention: SafeDelete <path> -> : <reason>".
  New test `execute_plan_reports_rollback_step_failure_in_error_message`
  proves this: a plan whose primary step fails, with two rollback steps (one
  that succeeds, one pointing at a nonexistent path that fails) -- asserts
  the surfaced error names the failing path, and that the rollback step
  which *could* succeed still ran and cleaned up its target despite the
  other one failing.
- Existing test `execute_copy_verify_plan_rolls_back_staged_file_on_short_copy`
  continues to cover the short-read case (verified still passing).
- Full workspace `cargo test --workspace --all-targets --locked`,
  `cargo fmt --all -- --check`, and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` all green
  after this change (111 tests now passing in `rt-storage` alone, up from
  109).

Not yet evidenced (why this stays "In progress", not "Resolved"): permission
failure, destination-full, resume-after-partial-completion, and
idempotent-retry tests from the original acceptance list are still missing.
`execute_storage_plan_with_checkpoints` already has a `completed_steps`
resume parameter (pre-existing), but nothing exercises resuming a plan that
was interrupted mid-way, and nothing simulates `EACCES`/`ENOSPC` from the
underlying filesystem calls. TNG-002 (storage moves racing active writes) is
a separate, still-fully-open finding -- this item is scoped to
copy-verify-rollback correctness only, not concurrency safety.

Required action (remaining): permission-failure and destination-full
simulation tests (likely via a restricted-permission tempdir or a mock/small
tmpfs quota, since Rust has no portable disk-full injection); an explicit
resume-after-interruption test using the existing `completed_steps`
parameter; an idempotent-retry test (running the same plan twice after a
partial failure doesn't corrupt state further).

Acceptance: bit-flip (done), short-read (done, pre-existing), permission
failure (missing), destination-full (missing), partial rollback (done),
resume (missing), and idempotent retry (missing) tests.

### TNG-004 — Torrent-controlled integers can wrap or overflow

**Status: In progress** · **Priority: P0** · **Confidence: high**

Verified evidence: `rt-metainfo` now rejects a piece-hash count that doesn't
match `ceil(total_length / piece_length)` (caught a real bug in
`rt-migrate/tests/scale.rs`'s own fixture generator, which had always
generated a fixed 1-piece hash regardless of declared length -- fixed the
fixture, not the check). Not yet verified: checked non-negative/positive
integer helpers, checked offset arithmetic across the parser, explicit
limits on files/path components/trackers/webseeds/total nodes, or any fuzz
target. Acceptance criteria (`-1`, `i64::MIN`, overflow, zero, absurd
piece length/count, fuzz coverage) are not evidenced as tests yet.

Evidence: `crates/rt-metainfo/src/parse.rs` casts signed bencode integers to
unsigned values and later accumulates offsets. Piece length is later narrowed
to `u32`.

Required action: checked non-negative and positive integer helpers; checked
offset arithmetic; explicit limits for raw input, files, path components,
trackers, webseeds, pieces, and total collection nodes; reject invalid or
unrepresentable values before allocation.

Acceptance: `-1`, `i64::MIN`, overflow, zero, absurd piece length/count, and
large tracker/webseed corpus tests, plus fuzz target coverage.

### TNG-005 — Outbound tracker/webseed egress policy is not wired

**Status: In progress** · **Priority: P0** · **Confidence: high**

Verified evidence: wiring is broader than it first looked. Every named
unguarded call site now routes through `self.egress_policy` in
`torrent_task.rs` -- `announce_http`, `announce_udp`, `scrape_tracker`, and
`fetch_webseed_block` all call `egress_policy.http_client(...)` or
`.resolve_and_validate(...)` with an `OutboundTargetKind`; the free-function
`metadata_task::announce_tracker` (its own separate client, used during
magnet metadata fetch) takes and uses one too. Not yet verified: that
`OutboundEgressPolicy`'s own implementation actually rejects
private/loopback/link-local targets and DNS-rebinding, bounds
redirect-chains/body-size/time, and reuses clients rather than
constructing one per call -- i.e. the wiring is real, but the policy body's
correctness is not yet independently checked. No negative-path tests
(loopback, RFC1918, link-local, rebinding, oversized body) found yet.

Evidence: `crates/rt-engine/src/egress_policy.rs` contains a policy type but no
live call sites. Tracker, scrape, and webseed paths in `torrent_task.rs` use
hostname resolution or a fresh HTTP client without private-address,
redirect-chain, response-size, or connection policy enforcement. Bodies are
fully collected before parser limits apply.

Required action: route every tracker, scrape, webseed, and metadata URL through
one server-owned policy; resolve and validate every address and redirect;
bound headers/body/decompression/time; reuse clients; emit rejection metrics;
default private/local ranges and metadata endpoints to denied.

Acceptance: loopback, RFC1918, link-local, IPv6-local, rebinding, redirect,
oversized-body, compressed-body, timeout, and allowed-public-target tests.

### TNG-006 — Authentication is fail-open and inconsistent across facades

**Status: In progress** · **Priority: P0** · **Confidence: high**

Verified evidence: Transmission and Deluge compat routers each gained a new
`route_layer` auth-guard middleware (token-or-cookie, matching the native
API's pattern), each with its own `AppState::with_engine_and_tokens`
constructor threading `api_tokens` through. qBittorrent-compat's
pre-existing guard was fixed to allowlist `/auth/login` and `/auth/logout`
(previously would have 401'd the login request itself) and to
percent-decode cookie values. Regression test added/fixed for Transmission
(`transmission_router_enforces_configured_token`; was a test bug, not an
implementation bug -- see burn-down log). **rt-api-rtorrent got no auth
guard at all** -- the finding's claim that it "is unsafe when mounted
without the daemon's outer guard" is still true and unaddressed. Not
verified for any facade: the qBit static `SID=torrentng` session-cookie
claim, CSRF/origin enforcement, cookie flag/expiry correctness, the native
sample's public `0.0.0.0` bind with `change-me`, or a public-bind
placeholder-token startup rejection.

Evidence: empty token lists disable guards in `torrentngd`, native, and qBit
routers; the native sample binds `0.0.0.0` with `change-me`; the qBit login
returns a static `SID=torrentng`; Transmission/Deluge/rTorrent routers are
unsafe when mounted without the daemon’s outer guard; CSRF claims are not
enforced.

Required action: one server-owned auth middleware with explicit local-dev
opt-out; reject public binds with missing or placeholder credentials; make
compatibility login/session behavior token-aware; protect every facade and
mutating route; implement CSRF/origin policy where the API claims it; use
secure, bounded cookies.

Acceptance: auth-on/auth-off tests for every facade, public/bind matrix,
placeholder-token startup failure, cookie flags/expiry, CSRF negative tests,
and direct-router mounting tests.

### TNG-007 — Peer ingress has no effective global unauthenticated budget

**Status: In progress** · **Priority: P0** · **Confidence: high**

Verified evidence: `Engine`'s accept loop now calls both
`peer_ingress.try_begin(peer_addr, ...)` (per-IP/handshake admission) and
`self.network_budget.try_acquire_peer()` (global slot) before spawning a
handshake task, on the rejection path releasing cleanly (no task spawned).
New `network_budget.rs` module (`GlobalNetworkBudget`, `SharedRateLimiter`)
has its own unit tests, including cross-clone slot sharing. A real bug was
found and fixed in this module this session: `SharedRateLimiter` used
`std::time::Instant` instead of `tokio::time::Instant`, which live-locked
its own `#[tokio::test(start_paused = true)]` test and was hanging the
*entire workspace test suite* indefinitely -- fixed, see burn-down log. Not
verified: uTP incoming path parity, handshake-read bounded timeouts beyond
what TNG-018 covers, or the acceptance suite (slowloris, burst, per-IP,
global-cap, malformed, permit-release-under-load).

Evidence: `PeerIngressBudget` exists in `crates/rt-engine/src/peer_ingress.rs`
but has no engine call site. Inbound connections are spawned from
`crates/rt-engine/src/engine.rs`; TCP/uTP handshake reads lack bounded
timeouts, and configured limits are not runtime enforcement.

Required action: wire a shared accept semaphore and per-IP budget into TCP and
uTP; cap the number and duration of unauthenticated handshakes; release all
permits on every failure path; add malformed/slow-peer metrics and bounded
penalty state.

Acceptance: slowloris, burst, per-IP, global-cap, malformed, timeout, uTP, and
permit-release tests under concurrent load.

### TNG-008 — Persistence updates are not transactional with runtime state

**Status: In progress** · **Priority: P0** · **Confidence: high**

Original evidence: add and update paths mutate the registry before later
DB/blob work; per-block transfer updates hold runtime and DB locks and
perform full upserts; job state/events are separate operations; migrations
apply DDL and update `user_version` separately.

Verified evidence (this session): fixed the specific, concrete "phantom
registry row" case the acceptance criteria lists first, for the highest-
traffic write path (`add_torrent`). `engine.rs`'s `add_torrent` was
confirmed to do exactly what the finding described: `reg.add(entry)` makes
the torrent visible to any concurrent reader (list/get) *before*
`save_torrent_blob` (disk write) and `persist_entry` (DB upsert) run, and
neither failure path rolled the registry entry back -- a blob-write or
DB-upsert failure left a torrent visible via the API with no blob, no DB
row, and no way to ever load its metadata again. Fixed by rolling back the
registry entry on either failure, and additionally cleaning up the
now-orphaned blob file if the blob write succeeded but the DB upsert
failed afterward (best-effort, logged if cleanup itself fails). Two new
regression tests: one forces the blob write to fail (occupying
`session_dir/torrents` with a plain file instead of a directory) and
confirms no phantom registry row remains; the other forces the DB upsert
to fail (`PRAGMA query_only = ON` on the connection) and confirms both the
registry row and the orphaned blob are cleaned up. Verified both are real
regression tests, not tautologies: temporarily disabled the rollback logic
and confirmed both tests fail before restoring the fix.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-engine` 129 tests, up from 127).

Not yet evidenced (why this stays "In progress", not "Resolved" -- this
finding's full scope is large): `add_magnet` and the update/tracker/label
paths were not audited or fixed for the same before-DB-write registry
mutation pattern; per-block transfer updates' full-upsert-per-block cost
(a write-amplification/scale concern, not a phantom-row one) is untouched;
job state and session-event writes are still separate, non-atomic
operations; migration DDL + `user_version` advancement is still two
separate steps; there is no startup reconciliation pass that detects and
repairs state left inconsistent by a crash between these steps (this
session's fix prevents *new* phantom rows going forward, it does not
repair any that already exist on disk from before the fix).

Required action (remaining): audit and apply the same
registry-add-then-rollback-on-failure pattern to `add_magnet` and any
other path that adds a registry row ahead of durable writes; batch/coalesce
per-block transfer persistence; make job-state + event writes atomic with
each other; make migration DDL + version bump transactional; add a startup
reconciliation pass for interrupted operations.

Acceptance: injected failure at every write boundary (partially done --
add_torrent's two failure points are covered; others are not), crash/restart
recovery (not done), no phantom registry rows (done for add_torrent; not
audited elsewhere), no lost transitions (not done), and bounded write
amplification (not done).

## P1 — runtime, scale, and lifecycle

### TNG-009 — “Global” peer and rate limits are per torrent/peer

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: same `GlobalNetworkBudget` as TNG-007 -- shared
(`Arc`-cloned) into every spawned `TorrentTask`, with engine-level
`set_download_limit`/`set_upload_limit` setters distinct from any
per-torrent value. Both directions are actually gated on real payload
bytes: `handle_block` calls `self.network_budget.download().acquire(...)`
per received block, and the peer-upload loop calls
`upload.global_upload.acquire(bytes).await` (a captured reference to
`network_budget.upload()`) before sending each `Piece` message, layered on
top of the existing per-peer `wait_for_upload_budget`. (First pass at this
note incorrectly said upload wasn't gated -- missed the differently-named
local binding on first read; corrected after checking the actual send
path.) Not verified: protocol-overhead accounting, or the acceptance suite
(multi-torrent/multi-peer aggregate tests, runtime mutation tests, fairness
tests, metrics-matches-wire-bytes).

Evidence: `NetworkConfig` documents total limits, but
`Engine::spawn_torrent_task` passes the same value to every task. Download and
upload token buckets are created per torrent or per peer. Static global limit
fields have no production call sites; `max_connections` is projected but not
enforced.

Required action: create shared process-wide limiters/budgets owned by the
engine, define whether limits include protocol overhead, and apply them to
every ingress/egress path. Expose actual aggregate enforcement and current
usage.

Acceptance: multi-torrent and multi-peer aggregate rate/connection tests,
runtime limit mutation tests, fairness tests, and metrics matching observed
wire bytes.

### TNG-010 — Tiering and 100k scale architecture are not runtime-integrated

**Status: Open** · **Priority: P1** · **Confidence: high**

Evidence: `TierController`, dormant snapshots, timer wheel, and compact bitmap
are definitions/tests in `crates/rt-engine/src/tier.rs`; no engine integration
was found. Stats marks every torrent active. All persisted rows spawn tasks;
each scheduler creates multiple native threads by default.

Required action: integrate dormant/warm/hot lifecycle into restore, event
routing, tracker deadlines, peer promotion, stats, and shutdown; remove the
per-torrent thread multiplier; make memory/fd/task budgets explicit. If the
architecture cannot meet the target, delete the 100k claim and publish a lower
supported envelope.

Acceptance: 100k dormant, 1k hot, restart, promotion/demotion, fd/thread/task
counts, RSS, API latency, and recovery benchmarks on the release binary.

### TNG-011 — Storage jobs block the engine actor and lack control-plane routes

**Status: Open** · **Priority: P1** · **Confidence: high**

Evidence: native storage execute returns `202`, but the engine command path
invokes synchronous storage execution. Native routing exposes only `GET
/api/v1/jobs`; pause/resume/cancel are not complete native controls.

Required action: move blocking execution behind a bounded job worker pool;
persist and stream progress; implement authenticated get/pause/resume/cancel;
bound concurrency and shutdown behavior.

Acceptance: concurrent plan jobs do not delay health/torrent commands; restart
resume; cancellation; durable progress; worker saturation; bounded shutdown.

### TNG-012 — Shutdown and task health are not trustworthy

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: `torrentngd/src/main.rs` now listens for SIGTERM
(`tokio::signal::unix::signal(SignalKind::terminate())`) alongside Ctrl-C.
`EngineCmd::Shutdown` and `DhtCommand::Shutdown` were both changed from
fire-and-forget to carry a `oneshot::Sender<()>` reply, and the engine
awaits `torrent_tasks` joins with a bounded `timeout(...)` (aborting and
logging on timeout) instead of dropping handles. Full suite passes,
including `shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit`.
Not independently verified: `axum::serve`'s own graceful-shutdown hook (is
the HTTP listener itself included in the same bounded shutdown?), whether
health reflects task liveness (vs. just "engine handle exists"), or the
acceptance suite (SIGTERM/Ctrl-C/worker-panic/DHT-death/storage-failure/
shutdown-under-load tests).

Evidence: `torrentngd` listens only for Ctrl-C; `axum::serve` has no graceful
shutdown hook; `EngineHandle::shutdown` does not await; remove drops task join
handles; engine/DHT task death can leave a non-None handle and a superficially
healthy endpoint.

Required action: handle SIGTERM and Ctrl-C; propagate a bounded cancellation
token; await engine/DHT/task joins; make health reflect task liveness and
readiness; define stopped-announce and DB-drain deadlines.

Acceptance: SIGTERM, Ctrl-C, worker panic, DHT death, storage failure, and
shutdown-under-load tests with bounded completion and truthful health.

### TNG-013 — Stats, SSE, and list APIs scale with full scans

**Status: Open** · **Priority: P1** · **Confidence: high**

Evidence: stats asks every task sequentially with a 250 ms timeout; SSE
`torrent_delta` scans and serializes all torrents for every client every second;
native list returns an unpaginated bare vector while docs promise query/envelope
semantics.

Required action: introduce snapshot/versioned aggregation, pagination and
bounded projections, delta subscriptions keyed by changed torrents, client
backpressure, and documented compatibility behavior.

Acceptance: 1k/15k/100k latency and allocation tests, many SSE clients, slow
consumer behavior, stable pagination, and API contract tests.

### TNG-014 — Per-peer metadata/bitmap allocations threaten scale

**Status: In progress** · **Priority: P1** · **Confidence: high**

Original evidence: `torrent_task.rs` allocates a `Vec<bool>` piece map per
peer and clones piece bitmap/metadata into upload contexts.

Verified evidence (this session): fixed the one piece of this finding that
was safe to fix without touching the peer-wire protocol's message-passing
architecture. `UploadContext.piece_map` (and `TorrentTask.piece_map`) were
plain, owned `PieceMap` values; `upload_context()` -- called once per new
peer connection (accept/connect/uTP-accept) -- did `self.piece_map.clone()`,
a full deep copy of `PieceMap.files: Vec<FileSpan>` (scales with file
count, not piece count) for every single peer. `metadata` was already
`Option<Arc<Vec<u8>>>` (cheap to clone); `piece_map` was the real gap.
`PieceMap` is never mutated after construction (confirmed: no
`self.piece_map = ...` assignment anywhere in the file), so wrapping it in
`Arc<PieceMap>` is a pure, safe win -- every other call site
(`self.piece_map.piece_count`, `.piece_to_file_regions(...)`,
`upload.piece_map.validate_request(...)`, etc.) kept compiling unchanged
thanks to `Arc<T>`'s auto-deref; only the two struct field declarations and
two construction sites needed to change. New test
`upload_context_piece_map_is_shared_not_deep_cloned_per_peer` proves the
sharing property directly via `Arc::strong_count`/`Arc::ptr_eq` (a
compile-time-enforced property once `Arc`-wrapped, so no revert-and-check
was meaningful the way it is for a runtime-only bug fix).

Deliberately NOT touched in this pass, and why: `have_pieces: Vec<bool>`
(the actual "per-peer bitmap" in the finding's title) is genuinely mutated
per-peer as our own download progresses (`PeerCommand::Have` updates each
peer task's own copy independently, since each peer runs as its own
spawned tokio task communicating only via channels -- there is no shared
mutable state between them today). Sharing it safely would mean either a
new synchronized shared-bitmap type (lock contention on a hot path) or
bit-packing the existing `Vec<bool>` (an 8x density win, but touches every
read/write/len call site across `received`, `pieces`/`bitfield_to_pieces`,
`peer_has`, and `have_pieces` -- all peer-wire protocol-critical code where
an indexing mistake would silently corrupt Have/Bitfield messages).
That redesign needs its own dedicated pass with careful protocol-level
testing, not a fix squeezed into an already-large session.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-engine` 135 tests, up from 134).

Not yet evidenced (why this stays "In progress", not "Resolved"): the
`have_pieces`/`peer_has` bitmap sharing and bit-packing described above is
unaddressed; there is still no per-peer memory accounting or peer-count
cap tied to metadata/bitmap size; no memory-profiled 1k-hot/large-piece-
count benchmark exists (the new test proves the *mechanism* -- shared
allocation -- not measured RSS at scale).

Required action (remaining): bit-pack and/or share `have_pieces`/`peer_has`
across peer tasks (needs a concurrency-safety design, not just a type
swap); per-peer memory accounting; a peer-count/metadata-size cap; a real
memory-profiled benchmark at 1k+ hot peers and large piece counts.

Acceptance: memory-profiled 1k-hot/large-piece-count runs (missing) and
peer churn tests (missing) -- this session's fix reduces per-peer
allocation for one of the two implicated structures but does not itself
constitute the required benchmark evidence.

### TNG-015 — Webseed polling creates an idle tax

**Status: In progress** · **Priority: P1** · **Confidence: moderate**

Verified evidence: the webseed tick's `tokio::select!` arm is now guarded
(`_ = webseed_tick.tick(), if !self.paused && !self.meta.webseeds.is_empty()
&& !self.picker.is_complete() => ...`), so a torrent with no webseeds or
that's already complete no longer fires this timer at all. Not verified:
adaptive backoff or failure-deadline behavior for torrents that *do* have
webseeds but are currently failing, or the acceptance benchmark (idle CPU/
timer counts, webseed recovery timing).

Evidence: every torrent runs a 100 ms webseed tick even when no webseed work
exists.

Required action: arm timers only when webseed work is possible; use adaptive
backoff and failure deadlines.

Acceptance: idle-torrent CPU/timer counts and webseed recovery benchmarks.

## P1 — protocol and transfer correctness

### TNG-016 — Pure v2 completion is a capability lie

**Status: In progress (honesty fix chosen over full implementation)** · **Priority: P1** · **Confidence: high**

Verified evidence: the required action offered two paths -- implement it,
or "reject unsupported pure-v2 operations explicitly." This took the
second path. `Engine`'s taskless-v2 peer-transfer and tracker-lifecycle
branches now return `Err("pure v2 peer transfer is not implemented")` /
`Err("pure v2 tracker lifecycle is not implemented")` instead of a silent
`Ok(())`, and `native_engine_capabilities` was corrected:
`pure_v2_metadata_completion: false`, new `pure_v2_transfer: false`,
`storage_plan_controls: false`, `storage_throttled: false`. Three tests
that asserted the old silent-success behavior were updated to assert the
new explicit errors instead (all previously green tests were asserting the
capability lie was correct behavior -- see burn-down log). Full pure-v2
transfer/tracker implementation itself remains not done; that is now
honestly reflected rather than claimed.

Evidence: engine task startup accepts `TorrentMetaV1`; pure-v2 metadata is a
taskless/recheck placeholder while the native capability manifest claims pure
v2 metadata completion.

Required action: implement v2 piece-layer acquisition, verification, storage,
resume, and tracker/peer lifecycle, or remove the capability claim and reject
unsupported pure-v2 operations explicitly.

Acceptance: pure-v2 magnet, metadata completion, partial resume, payload
verification, seeding, export, and compatibility tests.

### TNG-017 — Peer rate snapshots and choker inputs are wrong

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: `PeerHandle` gained `downloaded`/`uploaded` monotonic
counters and `download_rate_window`/`upload_rate_window`/
`rate_window_started` fields; a new `record_peer_transfer()` helper updates
both the cumulative counter and the current window on every block
transferred, and peer snapshots now compute
`peer_rate(peer.upload_rate, peer.rate_window_started)` for both
directions (previously upload used raw un-rated block bytes directly).
Full suite passes. Not verified: whether the choker's actual ranking logic
was updated to consume these correctly (only confirmed the inputs feeding
it changed), or the acceptance suite (controlled-transfer-rate,
peer-ranking tests with nonzero monotonic values).

Evidence: peer snapshots report zero rates/counters; `PeerEvent::Uploaded`
uses raw block bytes where the choker expects throughput.

Required action: maintain monotonic counters and interval rates at one defined
sampling boundary; distinguish wire/payload bytes; test choker ranking.

Acceptance: controlled transfer-rate and peer-ranking tests with nonzero,
monotonic values.

### TNG-018 — Peer loops lack hostile-peer I/O limits

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: added `PEER_IDLE_TIMEOUT` (120s, checked against
`last_activity.elapsed()` in the peer loop -- real call site, not just a
declared constant), a bounded 10s timeout on the initial handshake read
(`tokio::time::timeout(Duration::from_secs(10),
framed.get_mut().read_exact(&mut hs_buf))`, previously unbounded), and a
per-peer upload request-rate budget (`PEER_UPLOAD_REQUEST_WINDOW` = 10s,
`MAX_PEER_UPLOAD_REQUESTS_PER_WINDOW` = 256, bails the peer connection over
budget). Not verified: whether request disk I/O is actually isolated off
the peer loop (the finding's specific complaint was inline disk I/O
blocking the loop) -- `read_upload_block` is `async` but that alone doesn't
establish it's non-blocking under real disk load. Acceptance suite
(slow-read, request-flood, oversized-message, scheduler saturation) not
evidenced.

Evidence: request disk I/O runs inline in the peer loop; no clear idle timeout
or per-peer request-rate budget exists.

Required action: isolate disk work behind bounded scheduling; cap outstanding
requests, message sizes, request rates, and idle time; apply peer penalties.

Acceptance: slow-read, request-flood, oversized-message, idle, and scheduler
saturation tests.

### TNG-019 — DHT resource and validation controls are incomplete

**Status: In progress** · **Priority: P1** · **Confidence: high**

Original evidence: DHT is IPv4-only in the live task; there is no effective
rate limit or outstanding expiry; transaction IDs are two bytes; response
source validation and global announced-peer caps are incomplete.

Verified evidence (this session): fixed the most severe issue -- confirmed
this was a real, exploitable gap, not just a hardening nice-to-have.
`crates/rt-engine/src/dht_task.rs`'s `handle_packet` accepted *any* KRPC
`Response`/`Error` whose transaction ID matched an outstanding entry,
**regardless of which UDP address the packet actually came from** --
merging its claimed nodes into the routing table and, for `get_peers`,
forwarding its claimed peers straight to the torrent, unconditionally. Worse,
transaction IDs were a plain sequential `u16` counter starting at `1` on
every daemon launch (`next_tx.wrapping_add(1)`), not random -- fully
predictable across restarts. Together this meant an off-path attacker
(no need to see our real traffic) could send a handful of forged UDP
packets with guessed low transaction IDs and inject fabricated DHT nodes
or, more seriously, fabricated `get_peers` results that the torrent task
would treat as real, connectable peers.

- `OutstandingQuery` now records the address a query was actually sent to
  and a `sent_at` timestamp. `Response`/`Error` handling looks up the
  transaction ID and requires the packet's source address to match before
  trusting anything in it; a mismatch (or an unrecognized transaction ID)
  is logged and dropped without touching the routing table, without
  consuming the real outstanding entry, and without forwarding anything to
  the torrent.
- Transaction IDs now start from a random `u16` seed per daemon launch
  (reusing `NodeId::random()`, already backed by `rand` inside `rt-dht`,
  rather than adding a new direct dependency) instead of always `1`. Still
  sequential *within* a session (an attacker who observes one ID can still
  predict the next), but the source-address check above is now the actual
  security boundary -- guessing IDs alone is no longer sufficient.
- Added `prune_stale_outstanding`, run on a new 10s tick, dropping
  outstanding entries older than 30s -- closes the unbounded-growth path
  from nodes (or an attacker) that never respond.
- Five new regression tests: a spoofed-source response is rejected without
  touching the routing table or consuming the real query; an unknown
  transaction ID is rejected; expiry sweep removes only stale entries.
  Verified `response_from_wrong_source_address_is_ignored` is a real
  regression test by temporarily disabling the address check and
  confirming it fails first.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-engine` 134 tests, up from 129).

Not yet evidenced (why this stays "In progress", not "Resolved"): DHT is
still IPv4-only in the live task (no IPv6 support at all, not just
untested); there is still no rate limiting on inbound DHT traffic (a flood
of valid-looking queries/responses is not bounded); global announced-peer
caps beyond the existing per-info-hash cap were not audited; token/address
binding for `announce_peer` (does the token actually bind to the
querying address, preventing a third party from replaying an overheard
token?) was not audited.

Required action (remaining): inbound DHT rate limiting; IPv6 support or an
explicit, documented scope decision to not support it; announce-token
binding audit; global (not just per-info-hash) announced-peer/table caps;
flood and restart tests.

Acceptance: spoofed response (done), transaction reuse (partially --
source-address check is now the real defense; ID predictability itself is
unchanged), timeout (done, via the new expiry sweep), flood (missing),
IPv6 (missing), table cap (missing), private torrent (pre-existing,
unaffected), and restart tests (missing).

### TNG-020 — Tracker and PEX protocol handling is partial

**Status: In progress** · **Priority: P1** · **Confidence: high**

Original evidence: tracker response integers are cast to unsigned types; UDP
announces use a fixed 1500-byte buffer/new socket per request and incomplete
interval/id fidelity; PEX parses IPv4 `added` but not IPv6/dropped peers.

Verified evidence (this session): fixed the two most concrete,
correctness-focused sub-issues.

- `AnnounceResponse::parse` (`crates/rt-tracker/src/response.rs`) cast
  `interval`/`min_interval`/`complete`/`incomplete` from the bencoded `i64`
  to `u32` with a bare `as` -- a negative or absurdly large value from a
  buggy or hostile tracker silently wrapped into an unrelated u32 instead
  of being rejected (the sibling `scrape_int` helper in the *same file*
  already did this correctly with `u32::try_from`, so this was an internal
  inconsistency as much as a bug). Now uses checked `u32::try_from`
  throughout: `interval` (required, drives real re-announce scheduling)
  fails the whole response on an invalid value; the three optional stats
  fields degrade to `None` rather than failing the response over a
  cosmetic field. New tests: `parse_rejects_negative_interval`,
  `parse_rejects_interval_overflowing_u32`,
  `parse_treats_out_of_range_optional_stats_as_absent`. (Checked the UDP
  tracker parser too -- `crates/rt-tracker/src/udp.rs` already reads
  fixed-width fields via `from_be_bytes`, not vulnerable to this same
  cast-wraparound class.)
- `parse_ut_pex_peers` (`crates/rt-engine/src/torrent_task.rs`) only parsed
  the ut_pex extension's `added` (IPv4, BEP 11) key -- `added6` (IPv6) was
  silently ignored, meaning peers on IPv6-only or dual-stack swarms
  advertised via PEX were never discovered through this path. Now parses
  both and returns the combined peer list. New tests:
  `parses_ut_pex_added6_ipv6_peers`, `parses_ut_pex_added_and_added6_together`.
  `dropped`/`dropped6` remain intentionally unparsed -- see the "not yet
  evidenced" note below for why.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-tracker` 59 tests, up from 56).

Not yet evidenced (why this stays "In progress", not "Resolved"): UDP's
fixed 1500-byte buffer and new-socket-per-request pattern is untouched
(a resource-efficiency/scale concern, not a correctness bug); UDP announce
interval/transaction-id fidelity was not audited beyond confirming the
basic field parsing is cast-safe. `dropped`/`dropped6` are deliberately
NOT parsed yet: BEP 11 defines them as informational (a peer reporting it
disconnected from an address), not a command, and this engine has no
existing mechanism to act on that signal safely -- wiring it in requires a
real design decision (what should "dropped" actually *do*: nothing but
logging, deprioritize the address, or something else) that a rushed
addition to this already-large session should not make unilaterally.

Required action (remaining): decide and implement `dropped`/`dropped6`
semantics; UDP adaptive/bounded framing and connection reuse; announce
interval/transaction-id fidelity audit; malformed/fragmentation/MTU and
retry/interval tests.

Acceptance: malformed/negative/large tracker responses (done for HTTP
announce; UDP not audited), fragmentation/MTU (missing), retry/interval
(missing), IPv6 PEX (done), dropped-peer (missing -- needs a design
decision first), and private-torrent (pre-existing, unaffected) tests.

## P1/P2 — API, configuration, and product truth

### TNG-021 — Native list API does not match its documentation

**Status: Resolved** · **Priority: P1** · **Confidence: high**

`GET /api/v1/torrents` now returns `{total, torrents}` with real
`limit`/`offset`/`filter`/`status`/`category`/`tag`/`sort`/`dir`/`reverse`
query handling (`limit` clamped 1..=5000), and is memory-bounded via an
`ApiSnapshot` `reserve_memory` lease sized to the actual page, not the
whole registry. Verified by fixing/writing tests this session:
`list_torrents_empty` and `list_torrents_with_entry` were asserting the old
bare-array shape (updated to the envelope); added
`list_torrents_reports_total_independent_of_page_size`, which seeds 3
torrents and asserts `total: 3` while `limit=1` bounds the returned page to
1 -- the specific acceptance-relevant behavior (total != page size) that
neither old test could have caught. All three pass. `docs/API.md:135`
already documents `Response: { total: int, torrents: TorrentRow[] }` --
implementation now genuinely matches the documented contract.

Evidence: `rt-api-native` list handler returns a bare unpaged vector, while
`docs/API.md` promises query parameters and an envelope.

Required action: choose and version the contract; implement pagination/filter
limits/envelope or correct the docs and compatibility tests. Do not leave both
surfaces claiming different behavior.

### TNG-022 — Compatibility mutations and in-memory state are too often inert

**Status: Open** · **Priority: P1** · **Confidence: high**

Evidence: compatibility routes accept semantics that are not applied to the
native engine; several operator-facing stores remain process-memory state.

Required action: route supported mutations to durable engine state; for
unsupported behavior return structured capability/no-op metadata, log it, and
document it; classify and persist operator-created state.

Acceptance: method-by-method mutation matrix with stateful round trips and
restart tests.

### TNG-023 — Capability and health manifests overclaim implementation

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: `native_engine_capabilities` downgraded
`pure_v2_metadata_completion`, `storage_throttled`, `safe_move`,
`safe_delete_after_dry_run`, `bounded_shutdown`, and `scale_certification`
from `true` to `false`, and added `pure_v2_transfer: false` /
`storage_plan_controls: false`. Cross-checked each against this session's
own findings: all six downgrades correctly track ledger items that are
still Open or only In progress with acceptance criteria unmet (storage
authority/race/verification = TNG-001/002/003, still Open; storage job
async pool = TNG-011, still Open; shutdown = TNG-012, In progress but not
acceptance-complete; scale = TNG-010/026, still Open/Blocked) -- i.e. this
wasn't a blanket "set everything false," it's consistent with the real
state. Not yet done: the `implemented`/`enabled`/`certified`/`experimental`
state separation the required action calls for (currently still one flat
bool per capability), and no capability contract test exists that would
fail if a claim regresses without evidence.

Evidence: native capabilities hard-code pure-v2, durable job controls, bounded
shutdown, DHT, and scale claims that the runtime/evidence does not establish.

Required action: derive capabilities from active runtime paths/configuration or
remove the claims. Separate `implemented`, `enabled`, `certified`, and
`experimental` states.

Acceptance: capability contract tests that fail when a claim lacks a live
implementation and evidence reference.

### TNG-024 — Deployment defaults are unsafe, inconsistent, or silently ignored

**Status: In progress** · **Priority: P0/P1** · **Confidence: high**

Verified evidence: `Config::validate()` (called from the real config-load
path, `rt-config/src/lib.rs:289`) now unconditionally rejects any
`api_tokens` entry matching a placeholder pattern (`change-me`, `changeme`,
`replace-me`, or anything containing `REPLACE_WITH`, case-insensitive) --
stricter than the required action asked (it's not scoped to public binds
only), plus a separate check that a public `daemon.api_bind` requires
non-empty tokens of at least 16 characters. Both are covered by existing
`ConfigError::Validation` tests, part of the passing suite. Sample configs
(`deploy/native/config.toml`, the Kubernetes `secret.yaml`) were updated
from `change-me` to `REPLACE_WITH_RANDOM_TOKEN`, which now actually fails
`validate()` instead of silently starting -- previously renaming alone
would have been cosmetic; here it's backed by real enforcement. Peer-port
inconsistency (`6881` in Docker/Kubernetes vs. the daemon's real default)
fixed to `44444` across `Dockerfile`, `service.yaml`, and
`statefulset.yaml`. Docker build now uses `--locked`. Not verified: Compose
files (not seen in the diff -- may still be inconsistent), whether an
*invalid* (not just placeholder) config load failure is visible/fails
closed rather than silently falling back to defaults, and whether
`trust_proxy_header` / other bind-adjacent settings were reviewed.

Evidence: native sample config binds publicly with a known token; compose,
Kubernetes, and config use inconsistent peer ports; invalid standard-path
config can fall back to defaults; native Docker build did not explicitly use
`--locked`.

Required action: safe default bind/credentials; reject placeholders on public
binds; make config load failures visible and fail closed; centralize port values;
lock builds and test rendered deployment manifests.

Acceptance: clean-container startup, invalid-config, public-bind, compose,
Kubernetes, and Docker build smoke tests.

## P1/P2 — release evidence and engineering system

### TNG-025 — Native CI does not enforce native quality

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified evidence: `.github/workflows/ci.yml` gained a `native-quality` job
(fmt check, `cargo test --workspace --all-targets --locked`, `clippy -D
warnings`) plus `cargo test` for the sidecar (was build-only before).
`.github/workflows/release.yml` got the identical `native-quality` job,
and both `native-binaries` and `linux-release-assets` now declare `needs:
native-quality` -- release cannot produce artifacts unless it passes.
Everything this gate runs was independently re-verified locally this
session and is green. Separately, `.gitlab-ci.yml`'s trivy container scan
was changed from `--exit-code 0` (report-only, never fails the pipeline)
to `--exit-code 1` (actually blocks on HIGH/CRITICAL CVEs) -- not one of
the 29 named findings but a real release-gate fix in the same spirit. Not
verified: MSRV is not pinned in the new CI steps (`dtolnay/rust-toolchain@
stable` tracks whatever's current, not the declared 1.88/1.97 -- worth
pinning explicitly so CI catches the next MSRV drift instead of only
catching it when someone runs it locally, as happened this session).

Evidence: `.github/workflows/ci.yml` does not run native workspace tests,
formatting, clippy, or sidecar/native integration together.

Required action: add a required native quality job and make release depend on
it; pin/declare the MSRV policy; run locked tests and lint on supported targets.

Acceptance: intentionally broken fmt/test/clippy changes fail CI.

### TNG-026 — Release evidence is stale or weaker than its claims

**Status: Open** · **Priority: P1** · **Confidence: high**

Evidence: certification reports are from May 2026; universal compatibility is
`PASS_WITH_SKIPS`, the 24h soak is stale/incomplete, security evidence covers
sidecar config rather than native deployment, scale evidence is synthetic, and
strict readiness fails.

Required action: refresh evidence against the exact release artifact/config;
make skipped legs explicit; block release on stale/unknown rows; separate
synthetic proxy evidence from real deployment claims.

Acceptance: one machine-readable clean release bundle or an explicit blocked
release report with owner/action/artifact for every remaining row.

### TNG-027 — Claimed fuzz/OpenAPI/idempotency coverage is not checked in

**Status: In progress** · **Priority: P1/P2** · **Confidence: high**

Original evidence: repository search found no checked-in fuzz targets,
OpenAPI source, or idempotency test harness despite documentation
references. There was an empty placeholder `fuzz/` directory (0 files) --
confirming this was planned but never actually built.

Verified evidence (this session): added the fuzz-target half of this
finding, verified working, not just scaffolded.

- `cargo-fuzz` was not installed on this machine; installed it (via nightly
  Rust, already present) specifically so these targets could be built and
  actually run locally before being claimed as working, per this session's
  own verification discipline.
- Two real `libFuzzer` targets in `fuzz/fuzz_targets/`:
  `parse_torrent.rs` fuzzes `rt_metainfo::parse_torrent` -- the entry point
  for every `.torrent` file this daemon ever reads, the single highest-value
  target since `.torrent` files routinely come from untrusted sources.
  `bencode_decode.rs` fuzzes `rt_bencode::decode`, the lower-level parser
  underneath it that also parses tracker responses and DHT KRPC messages.
  Both only assert "does not panic/crash" -- `Err` on malformed input is
  correct and expected.
- Actually ran both locally (`cargo +nightly fuzz run <target> --
  -max_total_time=15`): `parse_torrent` completed ~2.71M executions in 16s,
  `bencode_decode` ~2.46M in 16s, zero crashes on either. This is real
  evidence the harnesses build and run against the current parser APIs,
  not just that the scaffolding exists.
- Wired a new `fuzz-smoke` CI job (`.github/workflows/ci.yml`): installs
  nightly + `cargo-fuzz`, runs each target with a bounded 60s budget, and
  uploads `fuzz/artifacts/` (crash reproducers) via `actions/upload-artifact`
  on failure -- directly satisfies this item's acceptance criterion ("CI
  invokes each target with a bounded smoke budget and publishes artifacts").
  This job could not be run in this sandboxed session (no way to trigger
  the actual GitHub Actions runner here); the manual local runs above are
  the closest available verification that the underlying commands work.
- `fuzz/` is deliberately excluded from the main Cargo workspace
  (`Cargo.toml`'s `exclude`, matching the existing `sidecar` pattern) since
  `cargo-fuzz` requires nightly + sanitizer flags incompatible with the
  main workspace's stable build.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all still
green (the main workspace does not see `fuzz/` at all,
`cargo metadata --no-deps` confirms it).

Not yet evidenced (why this stays "In progress", not "Resolved"): OpenAPI
schema generation/validation is entirely untouched; idempotency/replay
tests for mutating endpoints are entirely untouched; the new CI job itself
has not been observed actually running in GitHub Actions (only verified
locally); fuzz coverage is currently limited to two parsers -- tracker
HTTP/UDP response parsing, DHT KRPC message parsing, and peer-wire message
parsing are all untouched and would each be reasonable additional targets
given they also handle attacker-reachable bytes.

Required action (remaining): observe the CI job actually run and pass/fail
correctly; add fuzz targets for tracker/DHT/peer-wire parsing; generate and
validate an OpenAPI schema; add idempotency/replay tests for mutating
endpoints (add/remove/move/etc.).

Acceptance: CI invokes each target with a bounded smoke budget (done,
pending an observed real CI run) and publishes artifacts (done, pending
an observed real CI run with an actual crash to verify the upload step).

### TNG-028 — Formatting, clippy, and MSRV are already red

**Status: In progress** · **Priority: P1** · **Confidence: high**

Verified locally (see "Current verified evidence" above for full detail):
`cargo fmt --all -- --check`, `cargo test --workspace --all-targets
--locked`, and `cargo clippy --workspace --all-targets --locked -- -D
warnings` all pass now, including on the actual declared MSRV toolchains
(1.88 main workspace, 1.97 sidecar -- both `rust-version` fields were
corrected from an untrue "1.80" to the real, verified floor). Two clippy
findings were fixed (too-many-arguments on an egress-policy-widened
function, a redundant `u32 -> u32` cast). Not yet done: CI does not pin
the MSRV toolchain explicitly (see TNG-025 note) -- until it does, "passes
the declared toolchain" is only established by this session's manual run,
not by an enforced, repeatable gate.

Evidence: baseline fmt fails in `rt-migrate`; clippy fails on sort idioms,
MSRV-incompatible `is_multiple_of`, and a large enum variant.

Required action: repair current failures, document the supported toolchain, and
make the checks required in CI/release.

Acceptance: local and CI `fmt`, locked test, and `clippy -D warnings` pass on
the declared toolchain.

## P2 — architecture and maintainability

### TNG-029 — The engine has poor fault/change isolation

**Status: Open** · **Priority: P2** · **Confidence: high**

Evidence: `Engine` is roughly 6.9k lines, `TorrentTask` roughly 4.9k, and
native handlers roughly 6.6k. Actor orchestration, persistence, storage,
trackers, peers, DHT, and API projection are tightly coupled.

Required action: create explicit seams for storage jobs, peer admission,
outbound policy, persistence transactions, runtime aggregation, and capability
projection; split modules only after contracts and tests exist.

Acceptance: subsystem tests can run without the whole engine; failures are
contained and health identifies the failed subsystem; no new cross-layer
global state.

## Claims to delete or downgrade now

Until the corresponding ledger item is resolved, these claims are not release
claims:

- “100k torrents” as a production capacity guarantee;
- “pure v2 metadata completion” as a supported native capability;
- “global” connection/rate limits when enforcement is per torrent/peer;
- “bounded graceful shutdown” without signal and join tests;
- “universal compatibility PASS” when rows are skipped or stale;
- “security PASS” when evidence was run against a different deployment mode;
- “storage plan safe” when execution authority still accepts caller roots;
- “fuzz/OpenAPI/idempotency covered” without checked-in targets and CI output.

## Burn-down log

| Date | Change | Evidence | Ledger impact |
| --- | --- | --- | --- |
| 2026-09-01 | Created this canonical ledger; captured remediation initiative. | Repository audit baseline above. | All findings explicitly tracked; unsupported claims downgraded. |
| 2026-09-01 | First remediation tranche (same-day, prior session): started TNG-004/005/006/007/009/012/015/016/017/018/021/023/024/025/028 work; new `network_budget.rs`, `egress_policy` wiring, per-facade auth guards, shutdown reply channels, capability-honesty downgrades, CI native-quality job. Left uncommitted with a hung test suite and 2 known-failing tests (a partially-applied clippy fix in progress). | Session transcript; working-tree diff at handoff. | Real progress on 15 items, but unverified and non-buildable as a checkpoint. |
| 2026-09-01 | Second session: resumed from the exact handoff point (verified via file content match + no live cargo process), found and fixed a livelock in `network_budget`'s rate limiter (`std::time::Instant` instead of `tokio::time::Instant`, invisible to production but hung the *entire* `cargo test --workspace` under `start_paused` tests), fixed 4 tests asserting old pre-honesty-fix behavior, fixed 1 test-fixture bug (piece-count mismatch, caught by real new validation), fixed 2 clippy findings, corrected both `rust-version` fields from an unverified/untrue "1.80" to the real verified floor (1.88 main, 1.97 sidecar -- transitive-dependency-driven, not first-party code). Independently spot-verified ~10 of the prior session's specific implementation claims against the actual diff rather than trusting the transcript narration (one self-correction recorded in TNG-009's note: initially misread upload rate-limiting as unwired due to an incomplete grep, corrected after checking the real send path). Updated 15 ledger items from Open to In progress or Resolved with cited evidence and explicit gaps; left 14 untouched items (TNG-001/002/003/008/010/011/013/014/019/020/022/026/027/029) as Open -- no work found on any of them; a closer pass then found real TNG-001 evidence that a first look missed and corrected its status. | `cargo test --workspace --all-targets --locked` (green, was hanging), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green), `cargo +1.88 test --workspace ...` (green), `cargo +1.97 test --manifest-path sidecar/Cargo.toml ...` (green), `cargo test --manifest-path sidecar/Cargo.toml --locked` (green, 75 passed). | Tree is a real, buildable, green checkpoint for the first time since remediation began. TNG-021 fully Resolved; other items moved Open -> In progress with specific verified evidence and specific remaining gaps recorded per item, so a future session can resume without re-deriving what's already true. Committed as `a479bf0`. |
| 2026-09-01 | Third session (same date, continuing "build it all out"): implemented TNG-003's two headline complaints for real. Added streaming SHA-1 content verification (`verify_content_matches`/`hash_file_sha1` in `crates/rt-storage/src/plan.rs`) so `copy_verify()` no longer trusts aggregate length alone. Rewrote `rollback_plan()` to return both succeeded and *failed* rollback steps (previously a failed rollback step was silently dropped via `.is_ok()`); failures are folded into the returned `StorageError` message since that is the only channel the existing caller (`engine.rs`'s `execute_storage_plan_job`) reads. Added `StoragePlanExecution::rollback_failures` + `rollback_fully_succeeded()`. Wrote two new targeted tests: a same-length bit-flip that length-only verification would have missed, and a rollback step that itself fails being surfaced in the error while the other rollback step still runs. | `cargo build -p rt-storage` (clean), `cargo test -p rt-storage --lib` (111 passed, up from 109, 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green). | TNG-003 moved Open -> In progress (not Resolved: permission-failure, destination-full, resume-after-interruption, and idempotent-retry tests from its acceptance list are still missing -- see item detail). |
| 2026-09-01 | Fourth session (same date, continuing "build it all out"): implemented TNG-002's quiesce/resume storage-transition protocol. New `TorrentCmd::QuiesceForStorageMove`/`ResumeAfterStorageMove` in `crates/rt-engine/src/torrent_task.rs`, handled in the main actor loop, inside `pending_recheck_control` (an in-progress recheck reads files too), and in `metadata_task.rs` (no-op for not-yet-materialized torrents). `engine.rs`'s `move_torrent_payload_files` and the generic `EngineCmd::ExecuteStoragePlan` handler (`POST /api/v1/storage/execute`) both now quiesce affected running tasks before touching files and resume them afterward. Wrote a real regression test using a genuinely spawned `TorrentTask` (not the taskless path) proving a live task's cached save_root is correctly re-pointed after a move and a post-move recheck finds the content at the new location -- verified this actually catches the bug by temporarily reverting the fix and confirming the test fails (`Downloading` instead of `Seeding`) before restoring it. While building that test, found and fixed a real, separate, pre-existing bug: `rt-session`'s state machine had no `(Seeding, Checking)`/`(Seeding, Downloading)` transitions, so rechecking an already-seeding torrent via the *existing* `TorrentCmd::Recheck` command could never have its outcome reflected in the registry (`set_state` silently discards `transition()`'s `Result`). Fixed with a regression test. | `cargo test -p rt-engine -p rt-session --lib` (rt-engine 127 passed, up from 126; rt-session 19, up from 18; 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green), `cargo test --manifest-path sidecar/Cargo.toml --locked` (green, 75 passed, unaffected). | TNG-002 moved Open -> In progress (not Resolved: live-peer move-under-transfer, cancellation, crash/restart tests still missing -- see item detail). Uncovered and fixed an independent state-machine bug along the way (recheck-of-seeding-torrent outcome was unobservable), which also directly strengthens TNG-002's and TNG-003's own recheck-after-move safety net. |
| 2026-09-01 | Fifth session (same date, continuing "build it all out"): fixed TNG-008's first concrete "phantom registry row" case in `add_torrent` (`crates/rt-engine/src/engine.rs`) -- `reg.add(entry)` made a torrent visible before its blob was written and its DB row upserted, and neither failure path rolled the registry entry back. Now both failure points roll back the registry row; a DB-upsert failure after a successful blob write also cleans up the now-orphaned blob (best-effort, logged on cleanup failure). Two new regression tests force each failure independently (blocking the blob directory with a plain file; `PRAGMA query_only = ON` on the DB connection) and confirm no phantom row remains -- verified both are real by temporarily disabling the rollback and confirming both tests fail first. | `cargo test -p rt-engine --lib` (129 passed, up from 127, 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green). | TNG-008 moved Open -> In progress. This is a deliberately narrow slice of a very broad finding -- `add_magnet` and other registry-mutating paths are not yet audited for the same pattern, and job-state/event atomicity, migration transactionality, per-block write amplification, and crash-restart reconciliation are all still open (see item detail for the explicit remaining list). |
| 2026-09-01 | Sixth session (same date, continuing "build it all out"): fixed TNG-020's two most concrete correctness sub-issues. `AnnounceResponse::parse` (`crates/rt-tracker/src/response.rs`) used bare `as u32` casts on bencoded `i64` interval/stats fields -- a negative or oversized value silently wrapped instead of being rejected, inconsistent with the sibling `scrape_int` helper in the same file which already did this correctly. Switched to checked `u32::try_from`: `interval` now fails the response on an invalid value, the optional stats fields degrade to `None`. `parse_ut_pex_peers` (`crates/rt-engine/src/torrent_task.rs`) only parsed ut_pex's IPv4 `added` key; added `added6` (IPv6) parsing so dual-stack/IPv6 swarms' PEX-advertised peers are no longer silently dropped. `dropped`/`dropped6` intentionally left unparsed -- BEP 11 defines them as informational only and this engine has no mechanism to safely act on them yet; wiring that in needs a real design decision, not a rushed addition. Five new tests total (3 tracker, 2 pex). | `cargo test -p rt-engine -p rt-tracker --lib` (rt-tracker 59 passed, up from 56; 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green). | TNG-020 moved Open -> In progress. UDP framing/connection-reuse, interval/transaction-id fidelity audit, and dropped-peer semantics remain explicitly open (see item detail). |
| 2026-09-01 | Seventh session (same date, continuing "build it all out"): fixed TNG-019's most severe issue -- confirmed it was a real, exploitable DHT-poisoning gap, not just missing hardening. `handle_packet` (`crates/rt-engine/src/dht_task.rs`) accepted any KRPC Response/Error whose transaction id matched an outstanding entry regardless of which UDP address the packet actually came from, merging its claimed nodes into the routing table and forwarding get_peers results straight to the torrent unconditionally; combined with transaction ids being a plain sequential counter starting at 1 on every launch (fully predictable across restarts), an off-path attacker with no visibility into real traffic could inject forged nodes/peers with a handful of guessed low IDs. Added `OutstandingQuery` (address + timestamp per sent query); Response/Error handling now requires the source address to match before trusting anything, dropping (and logging) a mismatch without touching the routing table or consuming the real pending query. Transaction ids now start from a random per-launch seed (reusing `NodeId::random()`'s existing `rand` dependency rather than adding a new one) instead of always 1. Added a 10s sweep pruning outstanding entries older than 30s, closing the unbounded-growth path from non-responding nodes. Five new regression tests; verified the source-check test is real by disabling the check and confirming it fails first. | `cargo test -p rt-engine --lib` (134 passed, up from 129, 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green, after fixing one clippy finding in a new test). | TNG-019 moved Open -> In progress. IPv6 support, inbound rate limiting, announce-token binding audit, and global table/peer caps remain explicitly open (see item detail). |
| 2026-09-01 | Eighth session (same date, continuing "build it all out"): built and verified real fuzz targets for TNG-027 -- the repository had an empty placeholder `fuzz/` directory (0 files), confirming this was never actually implemented. Installed `cargo-fuzz` (was not present) so targets could be built and run locally, not just scaffolded. Added `parse_torrent` (fuzzes `rt_metainfo::parse_torrent`, the entry point for every `.torrent` file this daemon reads) and `bencode_decode` (fuzzes the lower-level `rt_bencode::decode` also used by tracker/DHT parsing). Ran both locally: ~2.7M and ~2.5M executions in 16s each, zero crashes. Wired a new `fuzz-smoke` CI job with a bounded 60s-per-target budget and crash-artifact upload on failure. `fuzz/` excluded from the main Cargo workspace (matching the existing `sidecar` pattern) since cargo-fuzz needs nightly + sanitizer flags. While re-running a full clippy pass for this, discovered `.clippy.toml` still declared the pre-correction `msrv = "1.80"` from before this session's earlier MSRV fix (which corrected `Cargo.toml`'s actual `rust-version` to `1.88`) -- the stale value had been silently suppressing real, applicable MSRV-gated lint suggestions across the whole workspace the entire session. Fixed the declared MSRV and applied the ~19 newly-surfaced findings (manual modulo checks -> `.is_multiple_of()`, manual `chunks_exact(N)` -> `.as_chunks::<N>()`) across 8 crates, mostly via `cargo clippy --fix`, with the diffs spot-checked for correctness. | Fuzz targets run locally with real execution counts and zero crashes (see above); full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green), `cargo test --manifest-path sidecar/Cargo.toml --locked` (green, 75 passed); `cargo metadata --no-deps` confirms `fuzz/` is not part of the main workspace. | TNG-027 moved Open -> In progress (OpenAPI schema and idempotency tests remain entirely untouched; the new CI job has not yet been observed running for real, only verified locally -- see item detail). Also closed a real, if quieter, MSRV-consistency gap that had been masking lint coverage since the second session's TNG-028 work. |
| 2026-09-01 | Ninth session (same date, continuing "build it all out"): fixed the safe half of TNG-014. `UploadContext`/`TorrentTask`'s `piece_map: PieceMap` was deep-cloned (`files: Vec<FileSpan>`, scales with file count) on every new peer connection; `PieceMap` is never mutated after construction, so wrapped it in `Arc<PieceMap>` -- a pure, mechanical, low-risk win (every other read call site kept compiling unchanged via auto-deref). New test proves the sharing via `Arc::strong_count`/`Arc::ptr_eq`. Deliberately left `have_pieces`/`peer_has` (the actual per-peer *bitmap*, genuinely mutated independently per peer task today) untouched -- sharing or bit-packing it safely needs a concurrency-safety design and touches protocol-critical Have/Bitfield code across four call sites, which deserves its own dedicated pass rather than a squeezed-in change. | `cargo test -p rt-engine --lib` (135 passed, up from 134, 0 failed), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green). | TNG-014 moved Open -> In progress. The bitmap-sharing/bit-packing half of the finding, per-peer memory accounting, a peer-count cap, and a real memory-profiled benchmark all remain explicitly open (see item detail). |

## Release gate

The native release gate must fail while any P0 item is Open or while TNG-025,
TNG-026, or TNG-028 is Open. A production-scale claim additionally requires
TNG-010, TNG-013, and TNG-014 to be Resolved with release-artifact evidence.
