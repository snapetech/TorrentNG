# TorrentNG Backend Audit Burn-down

Status: **implementation burn-down complete; qualification gates active**
Baseline: 2026-09-01, `main`  
Scope: native Rust engine, native daemon/API, compatibility facades, storage,
deployment, CI, and release evidence.

This is the canonical remediation ledger for the principal-engineer / investor
audit. Older roadmap and certification documents describe intended or locally
tested behavior; they are not proof that a feature is wired into the live
runtime. An item is not complete until its code path, focused regression test,
and release evidence exist together.

## Executive decision

TorrentNG is not certified as a production-grade 100k-torrent deployment or as
a universally compatible client. The current source has materially closed the
functional storage, lifecycle, snapshot, and compatibility gaps, and the
release binary passes the local authenticated daemon smoke. The release
posture remains **do not make unqualified scale, security, pure-v2, or
universal-compatibility claims** until the explicitly external evidence exists.

The burn-down order is:

1. security and data integrity;
2. runtime limits, lifecycle, and scale;
3. API and compatibility truth;
4. deployment, CI, and independent release evidence;
5. architecture seams and maintainability.

Current execution priority is functional correctness and isolation. The
100k-hot, public-compatibility, real-device, and 24-hour-soak gates are
extended proof work, not the current implementation gate; they remain
explicitly open and must not be represented as completed by local unit tests
or a synthetic dormant corpus.

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
| native release workflow (baseline) | INCOMPLETE | Historical baseline: release built and smoke-checked the binary without native test, fmt, or clippy gates. |
| certification status | NOT CLEAN | Universal compatibility is `PASS_WITH_SKIPS`; 24h soak is stale/incomplete; strict readiness fails. |
| checked-in fuzz/OpenAPI/idempotency evidence | PARTIAL | Fuzz targets and bounded CI smoke commands are checked in; the native OpenAPI contract is now checked in; endpoint replay tests and an observed hosted-CI run remain evidence gaps. |

## Current verified evidence (2026-09-04 local / 2026-09-04 UTC)

The current source tree has been re-verified after the functional isolation,
durability, compatibility, and contract fixes recorded in the latest
burn-down entries. The checks below combine local verification with the
completed hosted CI run; external hardware, public-network traffic, and
long-soak results remain separate evidence gates.

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | Native workspace tests green, with only explicitly ignored real-device tests skipped. |
| `cargo test --manifest-path sidecar/Cargo.toml --locked` | PASS | 125 unit tests and 87 integration tests green; two synthetic benchmarks remain explicitly ignored. |
| `cargo fmt --all -- --check` | PASS | Workspace formatting is clean. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | Warnings-denied lint is clean. |
| `cargo +1.88 build/test --workspace --all-targets --locked` | PASS | Declared main-workspace MSRV build and tests green. |
| `cargo +1.97.0 build/test --manifest-path sidecar/Cargo.toml --locked` | PASS | Declared sidecar MSRV build and tests green. |
| declared `rust-version` (both `Cargo.toml`s) | **CORRECTED** | Was `1.80` in both, unverified and untrue. Neither workspace's *locked* dependency graph builds below 1.88 (main: `idna_adapter` needs rustc 1.86+, plus `edition2024` needs Cargo 1.85+) or 1.97 (sidecar: `libsqlite3-sys`'s build script uses `cfg_select!`, stabilized between 1.94 and 1.97). This is a transitive-dependency floor, not first-party code needing new syntax. Corrected both `rust-version` fields to `1.88` / `1.97` to match reality; this itself is TNG-028 acceptance criteria ("document the supported toolchain"). |
| GitHub Actions CI run `33916500668` | PASS | All 10 jobs passed on `8c46b61`: native quality, both MSRV jobs, fuzz smoke, sidecar, WebUI, dependency security, backup/restore, API/SSE load, and fault containment. |

The hosted CI workflow has now run successfully. The companion dynamic
CodeQL orchestration also passed all four analyses (`33916500079`). This proves the
repository gates execute on GitHub's runners; it does not prove that branch
protection requires them, and the repository's branch-protection setting must
still be reviewed separately.

Focused release evidence from 2026-09-04 is indexed in
[`BACKEND_BURNDOWN_RELEASE_20260902.md`](BACKEND_BURNDOWN_RELEASE_20260902.md).
The current release binary was built, launched with an isolated authenticated
config, exercised through native REST, qBittorrent REST, health, and metrics,
and terminated with SIGTERM. The local release gate passed its implementation
checks with warnings; strict readiness failed because external evidence is
stale, missing, or explicitly skipped. This is a deployment smoke result, not
a 100k capacity result.

The latest verification rebuilt `target/release/torrentngd` locally on
2026-09-04 from clean code commit `83b70ce`: 22,433,352 bytes, SHA-256
`ff94ede075f7541ef9eecf5418b1c31324fb1b6ca2648681d975b3e9cd048e73`.
The current release-binary smoke passed authenticated health, native list and
transfer, qBittorrent list and transfer, Prometheus metrics, and SIGTERM
clean exit in 462 ms. See the current local release gate and smoke report:
[`local-release-backend-burndown-final-20260904.md`](../certification/reports/local-release-backend-burndown-final-20260904.md),
[`backend-burndown-native-release-smoke-local-release-20260904T192950Z.md`](../certification/reports/backend-burndown-native-release-smoke-local-release-20260904T192950Z.md).
Full workspace tests, warnings-denied clippy, formatting, OpenAPI validation,
sidecar tests, the current security scan, and the universal-live local Docker
matrix are green. These are current local facts, not external production
evidence. The universal-live report is `PASS_WITH_SKIPS`: its 28 local Docker
cases pass in [`universal-live-current-20260904.md`](../certification/reports/universal-live-current-20260904.md).
The separate public Debian matrix now passes; real-device storage remains an
explicit skip.

The documentation and certification-harness follow-up is now `8c46b61`; it
does not change the daemon binary. The local release process also passed the focused fault and API-load gates:
[`backend-burndown-native-fault-live-current-20260904.md`](../certification/reports/backend-burndown-native-fault-live-current-20260904.md)
passed live SIGKILL/restart, injected SQLite failure/recovery, API
cancellation, and filesystem failure isolation; the deterministic worker
panic/cancellation/rollback report is
[`backend-burndown-native-fault-current-20260904.md`](../certification/reports/backend-burndown-native-fault-current-20260904.md).
[`backend-api-load-current-20260904-final.md`](../certification/reports/backend-api-load-current-20260904-final.md)
passed 204,936 requests from 32 JSON clients plus 8 slow SSE consumers over
30 seconds with zero errors (p50 4.41 ms, p95 8.05 ms, p99 10.08 ms).
RSS was sampled as an allocation proxy; this is not an allocator profile or a
representative public production workload.

The current full Docker interoperability matrix is
[`interop-matrix-20260904T195529Z.md`](../certification/reports/interop-matrix-20260904T195529Z.md).
It covers bidirectional transfers with qBittorrent, Transmission, Deluge, and
rTorrent, failure/recovery protocol cases, and native/qBittorrent/Transmission/
Deluge facade mutations.

The first live public-torrent matrix is
[`public-debian-interop-20260905T191253Z.md`](../certification/reports/public-debian-interop-20260905T191253Z.md).
It resolved the official Debian 13.6 netinst torrent, supplied its verified
metainfo to Rust and the four reference clients, transferred 791,674,880
bytes, reached 100% in Rust, and observed three reference-client peers. The
v1 info hash is `481b6e3617be4c88f96cb25e47c9d8272130071e`. This closes one
public-swarm evidence row; it does not establish universal compatibility.

The named public-torrent 24-hour soak is active under the launch record
[`PUBLIC_TORRENT_SOAK_20260905.md`](PUBLIC_TORRENT_SOAK_20260905.md). Its final
status is intentionally still open until the full 86,400 seconds and post-soak
checks complete.

The current external preflight is
[`external-evidence-preflight-public-soak-20260905T193325Z.md`](../certification/reports/external-evidence-preflight-public-soak-20260905T193325Z.md):
Docker, public opt-in, migration corpus, and the active soak are green. The
real-device target remains the single warning; the current public and soak
evidence is recorded above.

### Historical functional isolation checkpoint (2026-09-02)

This checkpoint predates the current release artifact recorded above. It is
retained to show the sequence of the remediation work; the current source and
release smoke supersede its artifact statement. The extended release/hot-set
proof gate remains deliberately deferred. The source then had
focused green coverage for the substantive seams: `rt-session` 24 tests,
`rt-storage` 118 tests, `rt-engine` 165 tests, native API 48 tests, and
qBittorrent API 62 tests. This checkpoint adds bounded initial SSE snapshot
chunks, atomic engine-actor liveness and task reaping, shutdown requeue of
durable storage work, asynchronous payload-delete finalization and recovery,
detached move planning and metadata/blob/webseed reads, detached raw-add
parsing and blob persistence, detached magnet parsing and blob persistence,
detached dormant-torrent promotion, detached storage-root capacity probes,
bounded active-peer collection, detached pure-v2 file verification, and a
bounded engine stats task-query deadline. Transfer-stat writes are now
coalesced instead of issuing a full torrent-row SQLite upsert per uploaded or
downloaded block, with forced progress/state flushes on shutdown and state
changes. Native facet aggregates and
qBittorrent peer logs now reuse bounded runtime/snapshot indexes rather than
independently scanning the live registry.

Those tests establish behavior and failure handling only. They do not update
the release digest or close the deferred 100k-hot, public, device, or soak
evidence rows.

### Historical continuation checkpoint (2026-09-02)

This section records the intermediate state from an earlier continuation. It
is retained for audit history and is superseded by the authoritative source
reconciliation below; do not use its old “still open” bullets as the current
ledger.

This checkpoint supersedes older per-item prose below where that prose still
says a now-implemented seam is absent. The work continued on code-level
correctness; extended capacity and hosted-deployment proof stayed deferred as
requested.

Implemented in the current source tree:

- qBittorrent URL torrent downloads use the engine-owned outbound egress
  policy and bounded streaming response reader; every HTTP facade has an
  explicit whole-request body limit.
- Upload and download rate windows are independent, so one direction cannot
  reset the other direction's sampling interval.
- DHT has bounded tracked torrents, per-info-hash query history, outstanding
  requests, transaction-id collision handling, failed-send cleanup, inbound
  rate/state limits, and process-wide announced-peer limits.
- Idempotency-key claim/replay/conflict handling is shared by native,
  qBittorrent, Transmission, and Deluge mutation routers. Successful replies
  replay; failed replies release the key; conflicting fingerprints are
  rejected.
- Peer bans are bounded, persisted in schema version 8, restored before the
  engine listeners start, enforced for inbound/outbound peer admission, and
  projected from the engine's authoritative state.
- Seed-ratio and seed-idle limits now pause completed torrents when reached.
  Queue/automatic-management and move-on-completion flags that have no engine
  implementation return explicit unsupported results in engine mode rather
  than reporting false success.
- Deluge auxiliary plugin configuration, plugin enable/disable, and Execute
  command writes no longer mutate process-memory facades and report success;
  they return explicit unsupported results. The enabled-plugin projection only
  reports the native Label and Notifications surfaces.
- Migration/schema startup work is transactional, persisted projections are
  reconciled, and the native OpenAPI contract is checked in with a standard
  library validation script.

Still genuinely open after this pass:

- TNG-001's Linux descriptor-relative implementation is present. Portability,
  adversarial race, and non-Linux evidence remain deferred; this is no longer
  an unimplemented storage-authority path.
- TNG-002/003/008/011 now have deterministic cancellation/failure coverage and
  a live release-daemon crash/restart, API cancellation, injected
  database-failure, and filesystem-failure matrix. Permission, disk-full,
  device, and broader deployment permutations remain deferred.
- TNG-004/005/007/009/014/018/019/020 need broader hostile-input, IPv6,
  overhead, per-peer memory, and transport acceptance coverage. The parser
  bounds, egress policy, budgets, packed bitmaps, and protocol paths are
  implemented within their declared scopes.
- TNG-006's direct `rt-api-rtorrent::execute_xml` library entry point is not an
  independently deployable HTTP boundary; that contract is now documented and
  externally integration-tested, while mounted daemon routes are guarded.
- TNG-013 has a local 32-client/8-slow-SSE release-process load result, but no
  representative production-corpus allocator profile or public-client load
  evidence. Arbitrary filter-index refresh remains linear by design.
- TNG-016 remains explicitly unsupported for pure-v2 transfer; this is a
  deliberate capability boundary, not a hidden implementation claim.
- TNG-023 still needs accepted certification evidence before its `certified`
  state can change. TNG-025/027/028 now have hosted CI evidence in run
  `33916500668`; branch-protection enforcement remains a repository-settings
  question. TNG-026 has current local release, fault, API-load, hosted
  repository-gate, and one official public-transfer result; device and soak
  evidence remain open.
- TNG-027 has real parser fuzz targets, local runs, and a passing hosted fuzz
  smoke; broader mutation replay remains evidence work.
- TNG-029's stated synchronous actor-owned persistence defect is resolved:
  production authoritative DB work crosses a bounded supervised worker and
  the live crash/cancellation/DB/storage fault matrix passes. The engine/API
  remain large modules, so deeper decomposition is non-release maintainability
  work.

Confidence: high for the implemented code paths and local verification;
moderate for the remaining acceptance gaps because they require hosted CI,
external hardware, real client traffic, or long-running fault/load evidence.

## Authoritative current source reconciliation

Updated after the current source pass, full local test matrix, warnings-denied
clippy, OpenAPI validation, sidecar tests, release build, authenticated
release-binary smoke, live fault matrix, API/SSE load, and local client
interoperability matrix on 2026-09-04 UTC.

The detailed TNG sections below are the original audit narratives and burn-down
history. Some of them intentionally describe the defect before it was fixed.
This table is the current disposition and supersedes an older status line or
acceptance statement in those historical sections.

`Implemented locally` means the production code path and focused regression
coverage exist in this repository. `Evidence deferred` means the code is not
being reopened merely to manufacture a proof gate; it requires hosted CI,
external hardware, real client traffic, a long soak, or a larger deployment.
`Explicitly unsupported` means the capability is intentionally not advertised
as implemented and the API returns a clear unsupported result where it is a
mutation.

The detailed finding sections below preserve the original defect narratives
and their acceptance checklists for audit traceability. Their current status is
the disposition in this table; prose that says a seam is absent is historical
and is superseded by the source reconciliation above.

| Finding | Current disposition | Remaining action |
|---|---|---|
| TNG-001 | Implemented locally for Linux production storage paths | Keep portability fallback scoped; adversarial race and non-Linux evidence deferred |
| TNG-002 | Implemented locally: quiesce/resume, async plans, stale-job guards | Broader disk/device deployment permutations remain external |
| TNG-003 | Implemented locally: checked verification, rollback, checkpoint recovery | Permission/space/device-failure permutations remain external |
| TNG-004 | Implemented locally: bounded parser and checked numeric conversions | Extend corpus/fuzz execution beyond current local targets |
| TNG-005 | Implemented locally: outbound policy, bounded fetches, redirect/address validation | Run hostile DNS/redirect/egress matrix |
| TNG-006 | Resolved for the declared auth/library boundary: mounted daemon routes are guarded and the public rTorrent library contract has an external integration test | Run authenticated public-client compatibility against the deployed process; do not expose the library helper as an unowned HTTP server |
| TNG-007 | Implemented locally: shared ingress budget and per-source admission cap | Run hostile connection-storm evidence |
| TNG-008 | Implemented locally: transactional projections, rollback, reconciliation, batched stats; live restart and DB-failure matrix passes | Broader crash-point and deployment permutations remain external |
| TNG-009 | Implemented locally: shared peer/rate budgets and uTP cap | Measure protocol overhead and fairness under load |
| TNG-010 | Implemented locally: runtime tiering, compact dormant state, deadline wheel | 100k/hot-set certification is explicitly deferred |
| TNG-011 | Implemented and locally fault-tested: detached bounded storage workers, dedicated DB connection, durable restart recovery, live cancellation, DB-failure, and filesystem-failure isolation | Hosted/device deployment evidence and broader disk/permission/space matrix remain external |
| TNG-012 | Implemented locally: actor liveness, task reaping, bounded shutdown; live dependency health and SIGTERM paths pass | Shutdown-under-load and deployment timing evidence remain external |
| TNG-013 | Implemented and locally load-tested: immutable snapshots, indexes, pagination, journals, bounded SSE chunks; 204,936-request many-client/slow-consumer run passes with zero errors | Representative production corpus, allocator profile, and public/client load evidence remain external |
| TNG-014 | Implemented locally: packed bitmaps and shared immutable piece maps | Run peer-count memory profile |
| TNG-015 | Implemented locally: guarded webseed timer and exponential retry backoff | Run idle/large-swarm benchmark |
| TNG-016 | Explicitly unsupported: pure-v2 transfer/completion is not claimed | No implementation action until a complete v2 transfer design exists |
| TNG-017 | Implemented locally: independent rate windows and choker inputs | Run controlled transfer proof |
| TNG-018 | Implemented locally: handshake, idle, request, and response budgets | Run scheduler-saturation evidence |
| TNG-019 | Implemented locally within declared IPv4 live-DHT scope: bounds, source checks, tokens, caps | Do not claim live IPv6 DHT; run hostile-input/load evidence |
| TNG-020 | Implemented locally: checked tracker values, bounded UDP handling, PEX add/drop parsing and handling | Run broad tracker/transport interoperability evidence |
| TNG-021 | Resolved: native list contract and bounded pagination agree | None beyond regression maintenance |
| TNG-022 | Implemented locally: durable categories/tags/bans and ban eviction; unsupported mode/plugin operations now fail explicitly | Keep projection-only compatibility behavior documented; run real-client matrix |
| TNG-023 | Implemented locally: implemented/enabled/certified/experimental assurance states are separate | Keep `certified` empty until external evidence is accepted |
| TNG-024 | Implemented locally: fail-closed config validation, secret-file support, deployment templates | Run deployment on the target orchestrator and inspect rendered secrets |
| TNG-025 | Resolved for the repository gate: native quality, clippy, MSRV, fuzz, release-smoke, security, backup, load, and fault jobs execute successfully | Branch-protection enforcement still needs repository-settings review |
| TNG-026 | Current release artifact was built from clean `83b70ce`, deployed locally, authenticated, smoked, backed up/restored, and shut down cleanly; hosted CI run `33916500668` also passed on `8c46b61` | One official public Debian transfer passes; real-device storage, the 24-hour soak, remaining public sources, and strict readiness remain external gates |
| TNG-027 | Resolved for the repository gate: fuzz targets, OpenAPI validator, idempotency tests, and hosted bounded fuzz smoke are green | Broader parser and mutation replay corpus remains optional evidence work |
| TNG-028 | Resolved for the repository gate: format, clippy, locked tests, and declared MSRV pass locally and in hosted CI | Branch-protection enforcement still needs repository-settings review |
| TNG-029 | Resolved for the stated persistence-isolation finding: authoritative engine DB work uses a dedicated bounded supervised worker; live crash/DB/storage fault matrix and local client matrix pass | Full actor decomposition and deployment-specific fault evidence remain non-release structural follow-up |

The practical release statement is therefore: **local functional remediation,
live fault containment, API/SSE load, release-binary smoke, and the hosted CI
repository gate pass; branch-protection enforcement, real-device storage,
public compatibility, long-soak certification, and extended-scale proof are
not complete.**

## P0 — security and data integrity

### TNG-001 — Server-owned storage authority is bypassable

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

The implementation is complete for the supported Unix/Linux execution path.
`ServerStorageRoots::authorize_path()` rejects non-absolute paths and `..`
components, and `secure_fs` executes plan operations from already-opened root
descriptors with `openat`/`renameat`/`unlinkat`-style no-follow checks. The
shared `open_path_no_follow` path is used by scheduler/file-cache operations;
delete validates the save root and every resolved payload path. Add,
magnet-add, restore/startup, save-path updates, moves, rechecks, and storage
plans all use server-owned configured roots. Preview roots are not an
execution authority, and no configured writable root fails closed.

Focused coverage includes outside-root and `..` rejection, final and ancestor
symlink rejection, broken symlink handling, missing-ancestor creation, plan
execution, scheduler/file-cache opening, delete, and an ancestor replacement
regression on Linux. The remaining work is evidence: portability on other
platforms and a hostile concurrent filesystem-race run against a real mount.
Those are release qualification gates, not missing production wiring.

Acceptance for the implementation gate is met. Keep the Linux descriptor-
relative path as the production authority and do not broaden the non-Unix
fallback without equivalent no-follow guarantees.

### TNG-002 — Storage moves can race active writes

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

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

Deferred evidence (not an implementation blocker): the
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

Deferred evidence action: a live-peer move-under-transfer test (needs a
loopback peer-wire harness); cancellation and crash/restart tests around an
in-flight move; decide whether `Shutdown` should itself quiesce/wait on any
in-flight storage-plan execution rather than racing it.

Acceptance: move-under-download/upload, cancellation, crash, and restart
tests remain deferred live-evidence work; rollback and no-write-after-commit
behavior are implemented and covered by the focused storage/move tests.

### TNG-003 — Copy verification and rollback semantics are too weak

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

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

The original acceptance list is now covered locally for the repository-
deterministic cases: bit-flip, short-read, partial rollback, checkpoint resume
after a committed filesystem step, and idempotent retry all have focused
tests in `crates/rt-storage/src/plan.rs`. Permission-failure and destination-
full injection remain host/filesystem evidence because portable unit tests
cannot manufacture truthful `EACCES`/`ENOSPC` semantics; the real-root storage
certification runner is the correct gate for those failures. TNG-002 remains a
separate move-vs-active-peer qualification item, not an open TNG-003 code gap.

Acceptance: implementation and deterministic recovery coverage are complete;
permission, space, and device-specific behavior is external evidence.

### TNG-004 — Torrent-controlled integers can wrap or overflow

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Verified evidence: `rt-metainfo::parse_torrent` uses checked signed-to-unsigned
conversions, checked length/offset arithmetic, and explicit bounds for raw
metainfo, files, path components, trackers, webseeds, pieces, and collection
nodes. Piece-hash counts are checked against the declared piece length and
total length before allocation; persisted/runtime file reads are capped at
`MAX_TORRENT_BYTES`. Regression tests cover negative and overflowing integers,
zero/absurd sizes, path and collection limits, and piece-count mismatch. The
checked-in `parse_torrent` and `bencode_decode` libFuzzer targets also run in
the bounded fuzz CI job.

The implementation gate is complete, and hosted fuzz smoke is now green in CI
run `33915548520`. Broader corpus duration remains evidence work; malformed
input must continue to fail closed before any large allocation.

### TNG-005 — Outbound tracker/webseed egress policy is not wired

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Verified evidence: tracker announce/scrape, webseed reads, magnet metadata
tracker fetches, and qBittorrent URL torrent downloads use the server-owned
`OutboundEgressPolicy`. It resolves and validates every address, rejects
loopback/private/link-local/reserved targets by default, disables redirects,
uses bounded request/response time and body limits, and records rejection
metrics. The policy owns a bounded reqwest client cache rather than creating a
fresh unrestricted client per request. Focused tests cover denied local
targets, allowed public targets, redirect policy, body limits, and DNS
address validation.

The implementation gate is complete. IPv6-local and hostile DNS/redirect
behavior still need broader live-network evidence, but no production callsite
is allowed to bypass the policy.

### TNG-006 — Authentication is fail-open and inconsistent across facades

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Verified evidence: native, qBittorrent, Transmission, and Deluge mounted
routers use token-or-cookie authentication middleware, with login/logout
allowlisted only where the compatibility protocol requires it. Mutating
cookie-authenticated requests require the same-origin/CSRF policy. The
rTorrent library boundary exposes `execute_xml_with_token`; the unauthenticated
`execute_xml` helper is intentionally local-development-only and rejects
requests when the embedded `AppState` has configured tokens. Public binds
reject missing, short, or placeholder credentials during config validation;
qBittorrent `SID` cookies are bounded compatibility sessions and are not
treated as an authentication bypass.

Empty token lists are an explicit local/test opt-out. The implementation gate
is complete and mounted routes fail closed. The separate
`crates/rt-api-rtorrent/tests/library_entry_point.rs` integration test and
`docs/RTORRENT_LIBRARY_API.md` document that `execute_xml_with_token` is a
library entry point, not an independently deployable HTTP server. Cookie
attribute and public deployment behavior still need external client evidence;
there is no missing daemon route to add until a consumer supplies a server
adapter with its own bind, auth, limits, timeouts, and shutdown ownership.

### TNG-007 — Peer ingress has no effective global unauthenticated budget

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Verified evidence: the engine accept loop applies the shared process-wide
network budget and per-IP `PeerIngressBudget` before spawning TCP or uTP
handshake work. Rejected attempts release the per-IP permit, and handshake
reads/writes, peer-event delivery, and peer socket operations are bounded.
Global and per-source caps, malformed/slow-peer handling, and permit release
have focused tests; peer admission is also represented in engine health and
metrics.

The implementation gate is complete. A concurrent slowloris/connection-storm
run and broad uTP hostile-input evidence remain deferred deployment tests, not
an unwired budget path.

### TNG-008 — Persistence updates are not transactional with runtime state

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Original evidence: add and update paths mutate the registry before later
DB/blob work; per-block transfer updates hold runtime and DB locks and
perform full upserts; job state/events are separate operations; migrations
apply DDL and update `user_version` separately.

Verified evidence (baseline slice, now extended): fixed the specific, concrete "phantom
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

Full workspace tests, sidecar tests, format, compile, and strict clippy are
green; the current focused count is `rt-engine` 165 tests.

The same rollback/transaction pattern now covers `add_magnet`, metadata
completion, labels, mutable fields, tracker updates, and metadata-placeholder
state changes. Durable row/files/detail-tracker projections commit in one
SQLite transaction, and state transitions that emit a durable event append the
event in that same transaction. Registry changes are restored when projection
persistence fails. Same-state transitions are idempotent, and invalid
engine-side transitions are no longer silently accepted. Job state and job
events commit together across restart recovery, recheck
creation/progress/completion, storage-plan creation/checkpoints,
terminalization, and control operations. Torrent-task progress and recheck
progress use the same transactional job/event primitives.

Transfer statistics no longer perform a full torrent-row upsert for every
block or upload notification. The torrent task marks transfer state dirty,
flushes it on a bounded progress cadence, and forces a flush on state changes
and shutdown. The coalescing behavior has a regression test. Payload delete
also retains the registry/DB/blob projection until asynchronous cleanup
succeeds; failed cleanup resumes the torrent and remains retryable.

Remaining scope is failure evidence, not an unimplemented core boundary.
Migration DDL and `user_version` advancement are transactional and have a
rollback regression test. Startup reconciliation repairs missing/corrupt
metainfo/file projections or quarantines ambiguous filesystem state. Some
operator-only notifications intentionally have no paired row mutation and are
appended independently. The complete injected-failure matrix across all
filesystem, database, and event boundaries has not been run; this is a release
evidence gap, not a 100k proof requirement.

Acceptance for the implementation gate is met: no phantom registry rows, no
orphaned retry-inaccessible payloads, job state/events commit together,
transfer persistence is bounded, and crash/restart reconciliation repairs or
explicitly quarantines interrupted projections.

## P1 — runtime, scale, and lifecycle

### TNG-009 — “Global” peer and rate limits are per torrent/peer

**Status: Functional implementation complete; capacity evidence deferred** · **Priority: P1** · **Confidence: high**

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

The prior evidence above is historical. The current source uses one shared
`GlobalNetworkBudget` across spawned torrent tasks, gates payload download and
upload bytes at the shared buckets, and shares process-wide peer admission.
Per-torrent ceilings remain separate from global ceilings. Wire-overhead
accounting and fairness measurement are still evidence work.

Remaining action: measure protocol overhead, fairness, and the relationship
between reported counters and observed wire bytes under multiple torrents and
peers.

Acceptance: multi-torrent and multi-peer aggregate rate/connection tests,
runtime limit mutation tests, fairness tests, and metrics matching observed
wire bytes.

### TNG-010 — Tiering and 100k scale architecture are not runtime-integrated

**Status: Functional implementation complete; capacity evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence (2026-09-03): `runtime.torrent_tiers_enabled` now controls
the runtime path. Restore keeps paused/stopped/seeding/error rows in the
registry without parsing their metainfo blobs or starting torrent actors;
Downloading, Checking, and MetadataPending rows start actors. Lifecycle
commands and inbound TCP/uTP peers promote dormant rows, a shared deadline
wheel wakes due activity/deadline entries, and reconciliation processes at
most 256 promotions/demotions per tick while rescheduling the remainder.
Shutdown handles both hot actors and the storage worker, and engine stats
reports hot/warm/dormant counts. The
controller test tracks 100,000 registry keys with 2,000 hot entries and
enforces the two-percent/one-task proxy budget. Restore also bulk-repairs
missing tracker rows and authorizes configured storage roots once per restore,
instead of repeating that work per row.

This is runtime integration, not a 100k production claim. The registry stores
taskless rows as compact `DormantTorrent` records rather than full
`TorrentEntry` values. `DormantTorrentSnapshot` remains the separate tier
policy record; it retains only the information needed for activity and
deadline decisions. Persisted tracker deadlines are bulk-loaded into a shared
deadline wheel at restore and on demotion; due dormant seeds are promoted and
reannounced without a per-torrent timer task.
Registry aggregate counters and tier counts are maintained incrementally, so
engine stats no longer scans every registry entry just to calculate durable
totals or activity-tier totals. The API snapshots also advance from the
bounded mutation journal when it is retained, instead of reconverting every
registry row on each refresh.
The older production-daemon scale report records 100k restore, native/qBit
pagination, aggregate stats, restart, and one-torrent promotion/demotion
behavior, but it is tied to an older binary digest and remains historical
evidence. It does not exercise 1k/2k simultaneous hot torrents, real peer or
tracker traffic, or a soak.

No further implementation work is required for the current tiering seam. Keep
the registry projection and tier-policy projection explicit and preserve the
no-per-dormant-task/timer invariant.

Deferred proof gate: release-binary runs with 1k/2k simultaneous hot fixtures,
real metadata diversity, and host-specific RSS/fd/thread/latency measurements.
That work is intentionally not a prerequisite for this functional checkpoint.

Acceptance for the current implementation gate is met: dormant rows remain
addressable, promotion/demotion and restart preserve durable state, and no
per-dormant actor/timer is created. The deferred production-capacity claim
still requires a fresh scale run against the current release artifact.

### TNG-011 — Storage jobs block the engine actor and lack control-plane routes

**Status: Functional implementation and local live-fault evidence complete; external evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence (2026-09-03): filesystem plans now run behind a bounded
dispatcher (32 queued requests, two `spawn_blocking` workers by default), not
inside the engine actor. The dispatcher also enforces an end-to-end in-flight
cap equal to queued capacity plus worker slots, so paused/slow requests cannot
accumulate as unbounded supervisor waiters. Native save-path moves return
`202` with a durable job id; authenticated get/pause/resume/cancel routes
control the same job state. Plans, operation, affected torrents, and completed
checkpoints are serialized into job events. Pause waits asynchronously without
consuming a worker slot. Pause, cancellation, and shutdown controls are
checked before and between 64 KiB copy/hash chunks as well as at step
boundaries; staged partial output is rolled back when a step is interrupted.
Shutdown cancels/drains queued and active work. On restart, interrupted
storage jobs are requeued and queued/paused plans are reattached with their
validated checkpoint; non-storage jobs are paused.

The dispatcher now exposes retained, queued, capacity, and configured-worker
gauges through `EngineStats`/Prometheus. A registration guard also releases an
in-flight admission slot if the supervisor request future panics, with a
regression test covering a closed-worker failure path.

Production `Engine::start` opens a dedicated file-backed SQLite connection for
the worker supervisor, with WAL/foreign-key settings and a bounded busy
timeout; the actor's connection is no longer the worker's persistence mutex.
The live-start regression test verifies the worker health and capacity
projection before a clean engine shutdown. Payload deletion now follows the
same worker boundary: removal quiesces the torrent, queues a durable
root-confined delete, returns the job id, and finalizes metadata/registry
cleanup on completion. Recovery can reconstruct the delete target from the
durable job context even when the optional affected-torrent projection is
empty. Shutdown requeues active and queued work instead of terminally marking
it cancelled, while user cancellation remains terminal.

The production engine actor no longer owns the authoritative SQLite connection
or executes its torrent/job/state transactions synchronously. Those operations
cross `DbExecutor` into the bounded, ordered `DbWorker`, whose dedicated
blocking thread owns its connection and catches operation failures/panics. The
production storage dispatcher separately owns its checkpoint connection. Test
only direct SQLite helpers remain under `cfg(test)` so state-machine fixtures
can stay in-memory; they are not runtime escape paths.
Raw metainfo parsing and validated blob writes are detached before that
projection step. Dormant runtime-task promotion now reads, authorizes, and
parses metainfo on a coalesced blocking worker; the actor only installs the
prepared task and dispatches queued actions. DHT identity inspection for
resumed rows is also detached.

The worker test proves queue/slot behavior and durable state, and recovery now
reconciles real filesystem state against the persisted checkpoint before
reattaching a plan. A pause or cancellation arriving mid-step is observed at
the next bounded copy/hash chunk; atomic rename/unlink boundaries remain
indivisible, and failed staging is rolled back. The internal completion carries
terminal state, error, and completed-step details back across the actor
boundary. Save-path moves stop at a durable
`commit_pending` state after filesystem completion until the engine publishes
the new path to the torrent row; that state is recoverable after a crash, and
DB failure leaves the live projection on the destination instead of resuming
against the missing old path. Worker/actor persistence failures retain
`commit_pending` semantics and use bounded exponential in-process retries
before falling back to restart recovery. Checked-in Prometheus rules alert on sustained
saturation, an unhealthy supervisor, snapshot expiry, and SSE resync/lag
storms. Restart reconciliation detects and advances uncheckpointed
rename/copy steps, while ambiguous or corrupt staging state fails closed for
manual attention.

Per-torrent admission now rejects overlapping storage move/delete/recheck
operations using the durable active-job projection, and completion handlers
discard stale detached work instead of recreating a removed or paused
projection. This closes the move-vs-recheck and move-vs-delete races at the
engine boundary. Transfer-stat persistence is also coalesced in the torrent
task, so storage/network activity does not turn every block or upload into a
full SQLite row write.

Local implementation and live evidence now exist: the deterministic matrix
covers worker error/panic/cancellation, transaction rollback, storage-worker
panic/cancellation, liveness, and restart recovery; the release-daemon matrix
also passes SIGKILL/restart durability, API cancellation with source
retention, an externally injected SQLite trigger failure followed by recovery,
and an isolated filesystem failure. Broader permission/space/device and
deployment-specific failure permutations remain external evidence work.

Acceptance for the current implementation gate: concurrent plan jobs do not
delay health/torrent commands, payload deletion is asynchronous and
idempotent, shutdown preserves recoverable work, and durable progress survives
worker failure. Release-device and soak evidence remains deferred.

### TNG-012 — Shutdown and task health are not trustworthy

**Status: Functional implementation complete; evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: `torrentngd/src/main.rs` now listens for SIGTERM
(`tokio::signal::unix::signal(SignalKind::terminate())`) alongside Ctrl-C.
`EngineCmd::Shutdown` and `DhtCommand::Shutdown` were both changed from
fire-and-forget to carry a `oneshot::Sender<()>` reply, and the engine
awaits `torrent_tasks` joins with a bounded `timeout(...)` (aborting and
logging on timeout) instead of dropping handles. The daemon now passes a
shutdown future into `axum::serve`, and health checks engine liveness plus the
storage-worker and DHT dependency seams. Full suite passes, including
`shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit` and the
dead-dependency health test. The local release fault matrix now adds live
SIGKILL/restart, injected SQLite failure/recovery, filesystem failure
isolation, and clean shutdown. The remaining boundary is deployment-specific
shutdown-under-load and dependency-fault evidence.

The repository-local action is complete. The current fault matrix already
retains SIGTERM/restart, worker panic/cancellation, storage/DB failure,
DHT-death, and API cancellation evidence. Remaining work is target-deployment
shutdown-under-load measurement and physical disk/permission/space evidence.

Acceptance: SIGTERM, Ctrl-C, worker panic, DHT death, storage failure, and
shutdown-under-load tests with bounded completion and truthful health.

### TNG-013 — Stats, SSE, and list APIs scale with full scans

**Status: Functional implementation and local many-client/slow-consumer evidence complete; representative production evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence (2026-09-03): the native list API returns a bounded page and
immutable revision cursor; snapshots are cached for 750 ms, sort indexes are
lazy and shared, refreshes are single-flight, and an expired cursor returns
`410 Gone`. Native SSE sends one or more bounded initial snapshot chunks
followed by mutation-journal deltas and performs a bounded snapshot resync
only when the bounded journal expires.
When the journal still covers the cached generation, native and qBittorrent
snapshot refreshes apply only the changed hashes; they fall back to a registry
scan when there is no usable base snapshot or the journal has expired.
Engine stats uses a 500 ms cache, parallel task queries (up to 64), a 250 ms
aggregate task-query deadline, and per-query timeouts; native and qBittorrent
transfer-info now consume its aggregate rate/byte snapshot instead of walking
every torrent actor.
Durable torrent totals, byte totals, activity-tier counts, tracker status
counts, and active-job counts use maintained counters or aggregate SQL rather
than materializing every matching row.
The native qBittorrent facade's `/torrents/info` has the same pinned snapshot
and page index, returns `X-TorrentNG-Snapshot`, and `/sync/maindata` uses the
registry journal for changed/removed torrents. The sidecar TorrentNG backend
now carries that native snapshot token through every bounded sync page and
resilient sub-range retry; the sidecar qBittorrent backend also uses bounded,
hash-sorted `torrents/info` pages, but that external API has no server-side
snapshot, so its view is explicitly eventual and cleanup is skipped for a
cycle with page faults. The sidecar qBittorrent compatibility
`/sync/maindata` path now rejects full or incremental responses over 10,000
torrents with `413` instead of silently truncating a full sync or materializing
an unbounded delta. In sidecar mode that compatibility cursor is now a
durable SQLite revision with bounded deletion tombstones; wall-clock seconds
are not used, so same-second updates and removals cannot disappear between
polls. Large qBit projections skip
per-torrent live engine round-trips; durable fields and aggregate stats remain
available. Native SSE initial state is emitted as bounded chunks (default 500,
maximum 1,000) at one revision, with `snapshot_complete` framing; subsequent
events remain journal deltas.
qBittorrent `/log/peers` now queries only promoted runtime tasks, in parallel
with a bounded per-task deadline; dormant rows have no live peers and are not
walked one by one.

The redesign does not make every operation sublinear. A journal-driven refresh
still clones the immutable snapshot and rebuilds its filter indexes, native
`total` and arbitrary filters scan the snapshot, and the engine stats cache
still aggregates runtime state on expiry. Runtime task-stat collection is now
bounded by a 250 ms aggregate deadline in addition to per-query timeouts, so a
slow or dead task cannot hold the stats command indefinitely. qBit full
responses necessarily serialize their requested output and can be enormous;
large responses intentionally omit transient per-torrent tracker/swarm/limit
queries rather than issue 100k actor calls. SSE stream-instance drops are now
counted. The current release process load run used 32 JSON clients and 8
deliberately slow SSE consumers for 30 seconds: 204,936 requests, 249.6 MB of
responses, zero errors, p50 4.41 ms, p95 8.05 ms, p99 10.08 ms, and eight
successful SSE streams. RSS was sampled as an allocation proxy (119 samples,
18.9 MiB minimum, 23.7 MiB maximum, +4.85 MiB); this is not an allocator
profile, and the fixture had a small torrent corpus. Snapshot refresh/expiry,
journal resync/event/lag/disconnect/client counts, and estimated bounded
response volume are now exposed through Prometheus; checked-in alert rules
cover expiry/resync/lag thresholds. Cursor expiry, snapshot pinning, and
bounded SSE chunk framing have focused contract tests and the local process
load run, but not a representative production-corpus workload.
The release-optimized synthetic scale report
([`backend-burndown-scale-release-final-20260902.md`](../certification/reports/backend-burndown-scale-release-final-20260902.md))
records the current 1k/10k/15k, 50k, and 100k API/resource checks. The
production-daemon corpus report
([`backend-burndown-native-scale-release-final-20260902.md`](../certification/reports/backend-burndown-native-scale-release-final-20260902.md))
adds file-backed 100k restore, pagination, aggregate stats, restart, and
single-torrent tier-transition evidence. The current local concurrent-client
and slow-SSE report is
[`backend-api-load-current-20260904-final.md`](../certification/reports/backend-api-load-current-20260904-final.md).
The current Docker client/protocol matrix is
[`interop-matrix-20260904T195529Z.md`](../certification/reports/interop-matrix-20260904T195529Z.md);
it reconciles 10 base swarm, 4 extended, and 14 protocol cases as PASS after
qBittorrent unsupported-mutation assertion was corrected to require the
documented 501 response.
Snapshot filters/index rebuilds remain linear in the immutable snapshot, and
the local run does not replace representative production-corpus, allocator,
or public-client evidence.

Deluge and Transmission compatibility list calls remain a bounded full-list
fallback because their upstream RPC contracts expose no range or snapshot
cursor. rTorrent `d.multicall` has the same limitation. These legacy calls now
reject responses over 10,000 torrents with an explicit migration hint; they do
not pretend that client-side slicing is server-side pagination. Native and
qBittorrent endpoints remain the paged/snapshot-capable path.

The current implementation and local process-load gate is complete: the snapshot/index contract,
bounded SSE framing, cursor expiry, journal resync, and deliberately omitted
large-qBittorrent live fields are documented and covered by source-level
tests. Native sidebar media facets now use the same incremental snapshot index
instead of rescanning the snapshot; sidecar hot read paths use a bounded
blocking-DB gate, and sidecar log/stats probes keep filesystem reads behind
blocking boundaries. The remaining action is representative production-corpus
and allocator evidence, not another unbounded scan rewrite.

Deferred proof gate: representative list/stat/SSE corpus load, allocator
profiles, and current-artifact 1k/15k/100k capacity evidence. The local
many-client/slow-consumer gate is complete and the measurements remain open
only for production-representative certification.

### TNG-014 — Per-peer metadata/bitmap allocations threaten scale

**Status: Functional implementation complete; memory evidence deferred** · **Priority: P1** · **Confidence: high**

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

The earlier concern about mutable `Vec<bool>` maps is now closed: the engine
uses a private packed `PieceBitmap` for `peer_has` and `have_pieces`, while
wire/API boundaries still expand to ordinary bitfields or bool vectors. The
immutable `PieceMap` is also shared through `Arc` in upload contexts, so file
span metadata is not deep-cloned for every peer.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-engine` 135 tests, up from 134).

The remaining gap is measurement: there is no peer-count memory profile or
large-piece-count benchmark tying the packed representation to a deployment
budget. That is evidence work, not an unaddressed bitmap implementation.

Acceptance: the per-peer representation and queue-capacity behavior are
covered locally. A memory-profiled 1k-hot/large-piece-count run and hostile
peer-churn profile remain external measurement gates, not missing code.

### TNG-015 — Webseed polling creates an idle tax

**Status: Functional implementation complete; benchmark evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: the webseed scheduler is deadline-driven rather than a
fixed 100 ms interval. It stays asleep for paused, complete, peer-connected,
empty, or permanently failed webseed states; it wakes promptly when a seed is
ready, and applies exponential retry deadlines from one second to five
minutes. 404/410 responses advance failure state and successful fetches clear
the backoff. Focused retry/body/URL tests pass.

The remaining action is an idle/large-swarm benchmark measuring CPU wakeups and
recovery latency; the behavior itself is implemented and covered by focused
tests.

Acceptance: idle-torrent CPU/timer counts and webseed recovery benchmarks.

## P1 — protocol and transfer correctness

### TNG-016 — Pure v2 completion is a capability lie

**Status: Explicitly unsupported; honesty fix chosen over full implementation** · **Priority: P1** · **Confidence: high**

Verified evidence: the required action offered two paths -- implement it,
or "reject unsupported pure-v2 operations explicitly." This took the
second path. `Engine`'s taskless-v2 peer-transfer and tracker-lifecycle
branches now return `Err("pure v2 peer transfer is not implemented")` /
`Err("pure v2 tracker lifecycle is not implemented")` instead of a silent
`Ok(())`, and `native_engine_capabilities` was corrected to advertise
`pure_v2_metadata_completion: false` and `pure_v2_transfer: false`. Storage
plan controls and storage scheduling are separate implemented capabilities;
they are not implied by pure-v2 support. Three tests
that asserted the old silent-success behavior were updated to assert the
new explicit errors instead (all previously green tests were asserting the
capability lie was correct behavior -- see burn-down log). Full pure-v2
transfer/tracker implementation itself remains not done; that is now
honestly reflected rather than claimed.

Evidence: engine task startup accepts `TorrentMetaV1`; pure-v2 metadata is a
taskless/recheck placeholder while the native capability manifest claims pure
v2 metadata completion.

Current action: preserve the explicit unsupported boundary in the capability
manifest, API errors, compatibility docs, and regression tests. Implementing
pure-v2 piece-layer acquisition, verification, storage, resume, and
tracker/peer lifecycle is a separate product project, not an implied backend
capability or a prerequisite for this burn-down.

Acceptance: pure-v2 magnet, metadata completion, partial resume, payload
verification, seeding, export, and compatibility tests.

### TNG-017 — Peer rate snapshots and choker inputs are wrong

**Status: Functional implementation complete; evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: `PeerHandle` maintains monotonic uploaded/downloaded
counters and independent one-second rate windows. Block and upload events
update the counters at the torrent actor boundary; snapshots apply stale-rate
expiry, and the choker consumes the sampled upload rate rather than raw event
bytes. Global/per-torrent byte pacing remains separate from these telemetry
windows. Focused tests cover nonzero monotonic accounting and choker inputs.

The implementation gate is complete. Controlled multi-peer ranking and wire-
overhead measurement remain transfer evidence work.

### TNG-018 — Peer loops lack hostile-peer I/O limits

**Status: Functional implementation complete; evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: peer handshakes and socket writes have bounded deadlines;
idle peers expire, message/frame sizes are bounded by the wire codec, upload
requests have a per-peer rate cap and a bounded outstanding-read cap, and
upload disk reads are scheduled through the bounded mount scheduler in
detached futures. Peer-event delivery back to the torrent actor also has a
bounded send timeout, so a wedged actor cannot pin every peer loop. Focused
tests cover scheduler saturation and stalled event delivery.

The implementation gate is complete. Slow-read, request-flood, and hostile
transport load runs remain deployment evidence work.

### TNG-019 — DHT resource and validation controls are incomplete

**Status: Functional implementation complete within declared IPv4 scope; evidence deferred** · **Priority: P1** · **Confidence: high**

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

The implementation gap is now narrowed to declared scope and external proof.
The live DHT task remains IPv4-only for routing; IPv6 peer values can be
represented and forwarded, but IPv6 routing is not advertised as supported.
Inbound packets are bounded globally and per source IP, source-address and
token binding are enforced, tracked torrents/query history/outstanding
requests/announced peer sets have explicit caps, and stale outstanding work is
expired. Focused tests cover spoofed responses, transaction handling, timeout
expiry, per-IP/global flood budgets, global announced-peer caps, and token
validation.

Remaining action: keep the IPv4-only scope explicit and run broader hostile
input/load and restart evidence. Full IPv6 DHT routing is a separate feature,
not an unreported partial capability.

Acceptance for the current implementation gate is met for the declared scope;
IPv6 routing, live flood measurements, and restart evidence remain deferred.

### TNG-020 — Tracker and PEX protocol handling is partial

**Status: Functional implementation complete; interoperability evidence deferred** · **Priority: P1** · **Confidence: high**

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
  `dropped`/`dropped6` are also parsed and exposed as advisory removals to the
  torrent peer-discovery path; they do not force-close an established peer.

Normal announce and scrape I/O now runs in bounded, cancellable per-torrent
workers (maximum eight in flight) and reports back through a generation-guarded
channel, so a slow tracker no longer occupies the torrent actor's command
loop. Opaque HTTP tracker IDs are echoed on subsequent announces and persisted
in the tracker detail row across actor restart. Stopped announces remain
synchronous because shutdown/pause semantics require the actor to finish its
terminal notification before the state transition completes.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-tracker` 59 tests, up from 56).

The remaining action is broader transport evidence: UDP framing/socket reuse,
announce interval and transaction-id interoperability, malformed/MTU/retry
coverage, and public-client traffic. The implementation now has checked
tracker values, bounded tracker response handling, IPv4/IPv6 PEX additions,
and advisory dropped-peer handling.

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

That was the pre-remediation evidence. The current handler and OpenAPI
contract agree; retain the regression tests and treat any future contract
change as a versioned API change.

### TNG-022 — Compatibility mutations and in-memory state are too often inert

**Status: Functional implementation complete; compatibility evidence deferred** · **Priority: P1** · **Confidence: high**

Original evidence: compatibility routes accept semantics that are not
applied to the native engine; several operator-facing stores remain
process-memory state.

Verified evidence (this session): a targeted audit (not the full
method-by-method matrix the acceptance criteria calls for) found and fixed
the two highest-confidence, easiest-to-fix inert mutations -- both had an
already-working native-engine method one facade over, just never wired to
this one.

- rTorrent XML-RPC `d.tracker_announce` (`crates/rt-api-rtorrent/src/lib.rs`)
  was a pure literal `Ok(RtValue::Int(0))` -- it never even read `params`
  (which carries the target info hash), so a client asking rTorrent's
  "force reannounce" call for a specific torrent got a convincing success
  with nothing happening for *any* torrent. The qBittorrent-compat
  equivalent (`torrents_reannounce`) was already correctly wired to
  `Engine::reannounce_torrent`. Added a `tracker_announce` helper mirroring
  the existing `lifecycle` helper's hash-extraction pattern, now calling
  the same `Engine::reannounce_torrent`. New tests: missing/empty params
  now correctly error (previously silently "succeeded" for anything,
  including no hash at all); a valid hash with no engine attached still
  degrades gracefully, matching this crate's existing testing convention
  for engine-touching operations (no live-engine test harness exists in
  this crate; verified the underlying `reannounce_torrent` engine method
  itself is separately tested in `rt-engine`).
- Transmission RPC `session-set` for `dht-enabled`/`pex-enabled`
  (`crates/rt-api-transmission/src/lib.rs`) only mutated an in-process
  `AppState.session` struct (no DB backing); `session-get` echoed it
  straight back, so a client toggling DHT off and reading it back saw
  "yes, off" even though the swarm's real DHT/PEX state never changed.
  The qBittorrent-compat equivalent (`app_set_preferences`) was already
  correctly wired to `Engine::network_features`/`update_network_features`.
  Added the same read-current/apply-requested-fields/write-back pattern to
  `session_set`, alongside (not replacing) the existing process-memory
  mirror that `session-get` still reads from -- both stay consistent, but
  now the engine's real state changes too. Existing test
  `transmission_session_set_persists_broad_compat_settings_without_engine`
  (which already exercises `dht-enabled:false`/`pex-enabled:false`)
  continues to pass unchanged, confirming the no-engine path is
  unaffected; the qBittorrent-compat sibling this mirrors has no
  live-engine test of its own either (checked --
  `app_set_preferences_persists_form_and_json_updates` also runs without
  an engine and doesn't even exercise the dht/pex fields), so this fix's
  verification bar matches, and slightly exceeds, existing precedent in
  this codebase.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all green
(`rt-api-rtorrent` 19 tests, up from 17).

The earlier gap list is historical. The current source persists qBittorrent
categories and global tags in the native database, restores them across engine
restart, persists peer bans, restores bans before listeners start, and evicts
banned peers from active tasks on the tracker reconciliation path. Native
mode flags with no runtime equivalent (`force_start`, `auto_tmm`, and
`auto_management`) now return explicit unsupported results instead of storing
a value and claiming it changed behavior. Deluge plugin/configuration,
plugin-lifecycle, Execute-command, notification, and path-load operations and
Transmission utility gaps follow the same explicit-boundary rule. rTorrent
force-reannounce is wired to the engine.

This pass also removed the remaining Deluge auxiliary false-success path:
`blocklist.set_config`, `autoadd.*` writes, `scheduler.set_config`,
`extractor.set_config`, `execute.*` writes, and `core.*plugin` writes no
longer update process-memory state. Their read methods return documented
compatibility defaults, and only native Label/Notifications appear enabled.

The remaining action is a real-client compatibility matrix covering the
documented projection-only surfaces and unsupported responses. Move-on-
completion and blocklist/plugin behavior are not claimed as native features.

### TNG-023 — Capability and health manifests overclaim implementation

**Status: Functional implementation complete; certification evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: the native capability manifest now separates
`implemented`, `enabled`, `certified`, and `experimental` assurance states.
Runtime/config-dependent uTP fields are derived from active policy, while
pure-v2 transfer, IPv6 live-DHT routing, and scale certification remain
explicitly outside the advertised certified set. Contract tests cover the
manifest shape and mounted routes.

The remaining action is to keep `certified` empty for capabilities without
accepted external evidence and to update it only from a release/evidence
review, not from a local unit-test pass.

### TNG-024 — Deployment defaults are unsafe, inconsistent, or silently ignored

**Status: Functional implementation complete; deployment evidence deferred** · **Priority: P0/P1** · **Confidence: high**

Verified evidence: `Config::validate()` (called from the real config-load
path, `rt-config/src/lib.rs`) now unconditionally rejects placeholder tokens
and requires non-empty tokens of at least 16 characters for public binds.
Existing invalid config files no longer silently fall back to defaults;
defaults apply only when no config file exists. Native deployment templates
use the declared peer port, Docker builds use `--locked`, and rendered Compose
configuration validates locally.

Remaining action is target-orchestrator deployment and rendered-secret review.
The checked-in Kubernetes secret remains a template and must be populated by
the operator before apply.

## P1/P2 — release evidence and engineering system

### TNG-025 — Native CI does not enforce native quality

**Status: Repository gate resolved; branch-protection review outstanding** · **Priority: P1** · **Confidence: high**

Verified evidence: `.github/workflows/ci.yml` gained a `native-quality` job
(fmt check, OpenAPI validation, `cargo test --workspace --all-targets
--locked`, `clippy -D warnings`) plus formatting, tests, and clippy for the
sidecar (was build-only before). `.github/workflows/release.yml` got the same
combined native/sidecar gate plus an authenticated release-binary smoke using
the tracked `certification/fixtures/backend-burndown-native-release-smoke.toml`
fixture. Both `native-binaries` and `linux-release-assets` require the quality,
MSRV, and release-smoke jobs -- release cannot produce artifacts unless they
pass.
Everything this gate runs was independently re-verified locally this
session and is green. Separately, `.gitlab-ci.yml`'s trivy container scan
was changed from `--exit-code 0` (report-only, never fails the pipeline)
to `--exit-code 1` (actually blocks on HIGH/CRITICAL CVEs) -- not one of
the 29 named findings but a real release-gate fix in the same spirit.

Verified evidence (later session): closed the "MSRV is not pinned" gap
noted above -- which, in a nice bit of continuity, this same ledger had
already predicted would bite someone, and then did (see the `.clippy.toml`
staleness this session found and fixed under TNG-028's log entry). Added
two new CI jobs, `msrv-check` and `msrv-check-sidecar`
(`.github/workflows/ci.yml`), each pinning `dtolnay/rust-toolchain` to the
exact declared floor (`1.88.0` main, `1.97.0` sidecar) via `@1.88.0`/
`@1.97.0` version tags, and running a real build + full test suite at
that exact version -- alongside, not replacing, the existing `@stable`
`native-quality`/`sidecar` jobs (which still track current/future stable,
a distinct and still-valuable check). Verified both jobs' exact commands
locally against the already-installed `1.88` and `1.97.0` rustup
toolchains before committing: `cargo +1.88 build/test --workspace
--all-targets --locked` green, `cargo +1.97.0 build/test --locked
--manifest-path sidecar/Cargo.toml` green (75 sidecar tests passed).

Hosted evidence is now present: CI run `33915548520` passed all ten jobs on
`f1c39fd`, including native quality, both MSRV jobs, fuzz smoke, sidecar,
WebUI, dependency security, backup/restore, API/SSE load, and fault
containment. The dynamic `Push on main` orchestration also passed as run
`33915547352`. The remaining repository action is settings review: GitHub
branch protection is not evidenced as requiring these jobs.

### TNG-026 — Release evidence is stale or weaker than its claims

**Status: Local and hosted repository evidence current; external evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence (2026-09-04 UTC):
`target/release/torrentngd` was rebuilt from the clean `main` tree at commit
`83b70ce` with `cargo build --release --locked -p torrentngd`, launched with
an isolated authenticated config, exercised through health, native
list/transfer, qBittorrent list/transfer, and Prometheus metrics, and
terminated with SIGTERM.
The process exited cleanly. The exact current artifact and deployment report
are linked from `docs/BACKEND_BURNDOWN_RELEASE_20260902.md`.

The current artifact is 22,433,352 bytes with SHA-256
`ff94ede075f7541ef9eecf5418b1c31324fb1b6ca2648681d975b3e9cd048e73`; smoke
duration was 462 ms. Compose rendering also passes. The current source passed
the strict local fault matrix, live daemon fault matrix, 32-client/8-slow-SSE
load gate, and reconciled 28-case local Docker interoperability matrix. This
is current local deployment evidence, not a clean public release certificate.
The older 100k scale reports remain historical because they target an earlier
binary digest.

Remaining action is external release evidence: public/client compatibility,
real-device storage, and 24-hour soak. Extended scale proof is intentionally
deferred as a product-priority choice.

### TNG-027 — Claimed fuzz/OpenAPI/idempotency coverage is not checked in

**Status: Repository gate resolved; breadth evidence deferred** · **Priority: P1/P2** · **Confidence: high**

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
  Hosted run `33915548520` passed this job.
- `fuzz/` is deliberately excluded from the main Cargo workspace
  (`Cargo.toml`'s `exclude`, matching the existing `sidecar` pattern) since
  `cargo-fuzz` requires nightly + sanitizer flags incompatible with the
  main workspace's stable build.

Full workspace `cargo test --workspace --all-targets --locked`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` all still
green (the main workspace does not see `fuzz/` at all,
`cargo metadata --no-deps` confirms it).

The repository-side implementation and hosted gate are now complete: the two
parser fuzz targets run locally, the bounded fuzz-smoke job and crash-artifact
upload are checked in and green in run `33915548520`,
`docs/API.openapi.json` validates through `scripts/validate_openapi.py`, and
shared idempotency claim/replay/conflict tests cover the native, qBittorrent,
Transmission, and Deluge mutation routers. A broader parser and mutation
replay corpus remains optional evidence work.

### TNG-028 — Formatting, clippy, and MSRV are already red

**Status: Repository gate resolved; branch-protection review outstanding** · **Priority: P1** · **Confidence: high**

Verified locally (see "Current verified evidence" above for full detail):
`cargo fmt --all -- --check`, `cargo test --workspace --all-targets
--locked`, and `cargo clippy --workspace --all-targets --locked -- -D
warnings` all pass now, including on the actual declared MSRV toolchains
(1.88 main workspace, 1.97 sidecar -- both `rust-version` fields were
corrected from an untrue "1.80" to the real, verified floor). Two clippy
findings were fixed (too-many-arguments on an egress-policy-widened
function, a redundant `u32 -> u32` cast). The native-quality, MSRV, and
sidecar checks are defined in CI and pass in hosted run `33915548520`.
Repository branch-protection enforcement remains a settings review, not a
source-code gap.

## P2 — architecture and maintainability

### TNG-029 — The engine has poor fault/change isolation

**Status: Persistence-isolation implementation and local fault evidence complete; broader decomposition deferred** · **Priority: P2** · **Confidence: high**

Verified evidence (2026-09-04): explicit seams now exist for storage-job
dispatch/control/recovery, registry revisions and mutation deltas, native and
qBit snapshot projection, peer admission, outbound egress policy, process-wide
network budgets, storage-root authority, command replies, and capability
projection. Those seams have focused tests that do not require a live network
or the full daemon. A separate `peer_listener` task now owns TCP/uTP accept,
admission, and handshake work; it hands peers to the engine through a bounded
command queue, so the engine retains authoritative routing without cloning
the torrent-channel map for every connection. Health probes engine-owned
storage-worker and DHT dependency seams independently of the engine actor and
reports the peer-listener task separately; the fault test proves
a dead DHT channel and dead storage supervisor are reported as unhealthy.
Health exposes the current capability boundary instead of claiming scale
certification or unsupported storage behavior.

The storage worker now has a production-only database connection boundary,
and a real `Engine::start` test verifies that the supervisor remains healthy
and reports its bounded capacity through the command path before shutdown.
The engine actor has an explicit liveness guard and reaps failed torrent
tasks, while storage shutdown requeues durable work and delete recovery
finalizes metadata after payload cleanup. Native SSE initial snapshots are
bounded and registry mutations wake streams through a shared notifier.

The current functional isolation pass also adds per-torrent durable-job
admission guards, stale-completion checks for detached workers, transactional
job/event projection updates, rollback of registry projections when durable
writes fail, and coalesced transfer-stat persistence. These contain the most
dangerous move/delete/recheck and partial-projection races without pretending
that the actor has been decomposed. Storage plans now fail closed when a live
target cannot acknowledge quiescence; any targets already paused for that plan
are resumed before the error is returned. File-priority writes share the same
active-job admission gate, and generic move/delete plans require explicit,
registry-valid torrent targets because arbitrary filesystem paths cannot be
reliably attributed to a torrent by the engine.

The architecture is still a large actor monolith (`Engine`, `TorrentTask`,
and API handler modules remain oversized), but the highest-risk ownership
boundary is now structural rather than a naming convention. In
`crates/rt-engine/src/storage_control.rs`, storage-plan validation,
quiesce/submit/completion choreography, and resume-on-failure are isolated
from the general command dispatcher. In
`crates/rt-engine/src/subsystems.rs`, actor-owned torrent/tiering state is
separate from detachable DHT, storage-worker, budget, resource-governor, and
stats services. `Engine` remains the ordering coordinator; these modules do
not pretend that it has become a fleet of independently supervised actors.

Tracker announce and scrape transport is now a separate
`crates/rt-engine/src/tracker_runtime.rs` boundary: `TrackerWorkers` owns the
bounded per-torrent worker set, abort handles, generation fencing, HTTP/UDP
transport, response limits, and result channel. `TorrentTask` supplies an
immutable announce context and remains the ordering authority for tracker
state, tier failover, and peer admission. Cancellation on pause, quiesce,
shutdown, and session restart aborts those workers and drops stale results, so
a failed or slow tracker cannot retain actor-local protocol state or apply
peers after the session has changed. The stopped-announce path is still
intentionally actor-awaited, but its network work is bounded and parallel
under an aggregate deadline; that is the remaining shutdown/pause coupling.

The native engine's high-volume session-event writes use a bounded,
single-consumer writer that executes SQLite and retention pruning on a blocking
worker; session-log reads and the main operator read projections likewise run
outside the actor. All production authoritative torrent/job/state persistence
now crosses the same ordered `DbExecutor` boundary into `DbWorker`; the actor
does not retain the SQLite mutex. The worker owns a private connection,
bounded admission, cancellation fencing for queued work, panic containment,
health state, and drain-on-shutdown behavior.

Normal TCP/uTP accept failures keep control commands available while the
listener retries with bounded backoff; a listener-task exit marks readiness
false and is not auto-restarted. The local deterministic matrix passes worker
error/panic/cancellation, transaction rollback, storage-worker
panic/cancellation, liveness, and restart-recovery checks. The local release
daemon matrix passes SIGKILL/restart durability, API cancellation with source
retention, injected SQLite failure and recovery, isolated filesystem failure
with source retention, and health continuity; the current live report is
[`backend-burndown-native-fault-live-current-20260904.md`](../certification/reports/backend-burndown-native-fault-live-current-20260904.md).
The local API/SSE load gate
passes 204,936 requests from 32 JSON clients and 8 slow consumers over 30
seconds with zero errors. These are real local process checks, not a claim
that every dependency failure mode or public deployment has been certified.

Full inversion of tracker, peer, and every API dependency is not required to
resolve the stated TNG-029 persistence defect and remains a separate
maintainability choice. The remaining evidence is deployment-specific:
physical storage/device faults, public compatibility, and long-soak behavior.
Hosted repository CI is now green; branch-protection enforcement still needs
settings review.

Acceptance for the stated implementation gate is met: worker and engine
liveness is truthful, failed torrent tasks are reaped and projected as errors,
shutdown work remains recoverable, delete recovery is idempotent, API streams
have bounded initial event size, authoritative production SQLite work is
owned by supervised worker boundaries, and the local live fault matrix keeps
the daemon healthy across injected failures. Full actor decomposition remains
non-release structural follow-up.

## Claims to delete or downgrade now

Until the corresponding ledger item is resolved, these claims are not release
claims:

- “100k torrents” as a production capacity guarantee;
- “pure v2 metadata completion” as a supported native capability;
- “universal compatibility” across clients and transports without live
  interoperability evidence;
- “bounded graceful shutdown” without signal and join tests;
- “universal compatibility PASS” when rows are skipped or stale;
- “security PASS” when evidence was run against a different deployment mode;
- “storage plan safe” when execution authority still accepts caller roots;
- “fuzz/OpenAPI/idempotency certified” without hosted CI output and a broader
  replay corpus.

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
| 2026-09-01 | Tenth session (same date, continuing "build it all out"): closed a gap TNG-025's own entry had already flagged as a prediction -- MSRV wasn't pinned anywhere in CI (`@stable` tracks current, not the declared floor), which is exactly the class of drift that let `.clippy.toml` go stale earlier this session. Added `msrv-check`/`msrv-check-sidecar` jobs pinning `dtolnay/rust-toolchain` to the exact declared floors (1.88.0 / 1.97.0) via version tags, alongside (not replacing) the existing `@stable` jobs. Verified both jobs' exact commands locally against the already-installed pinned toolchains before committing: full workspace build+test green at 1.88, sidecar build+test green at 1.97.0 (75 passed). | `cargo +1.88 build/test --workspace --all-targets --locked` (green), `cargo +1.97.0 build/test --locked --manifest-path sidecar/Cargo.toml` (green, 75 passed), full default-toolchain `cargo test --workspace --all-targets --locked` / `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked -- -D warnings` (all green). | TNG-025's evidence updated with the MSRV-pinning fix; still not verified that any of this session's CI edits have actually run in real GitHub Actions (no way to trigger that from this sandboxed session). |
| 2026-09-01 | Eleventh session (same date, continuing "build it all out"): ran a targeted (not full-matrix) audit for TNG-022 via a research subagent, then fixed the two highest-confidence, easiest-to-wire inert compat mutations it found -- both had an already-working native-engine method one facade over, never connected to this one. rTorrent's `d.tracker_announce` (`crates/rt-api-rtorrent`) was a literal `Ok(Int(0))` that never read params at all; wired to `Engine::reannounce_torrent`, mirroring the already-correct qBittorrent-compat sibling. Transmission's `session-set` `dht-enabled`/`pex-enabled` (`crates/rt-api-transmission`) only mutated a process-memory struct that `session-get` echoed back convincingly; wired to `Engine::network_features`/`update_network_features`, alongside (not replacing) the existing mirror, mirroring `app_set_preferences`'s already-correct qBittorrent-compat pattern. The same audit surfaced several more inert mutations that were NOT fixed because they need real new engine features (peer banning/blocklist enforcement, a move-on-completion hook) rather than a wiring fix, plus a durably-stored-but-behaviorally-inert variant (`setForceStart`/`setAutoTMM`/`setAutoManagement` persist but `apply_torrent_limits()` never reads them) -- all recorded as explicit remaining gaps. | `cargo test -p rt-api-rtorrent --lib` (19 passed, up from 17), `cargo test -p rt-api-transmission --lib` (32 passed, 0 failed, no regressions), full `cargo test --workspace --all-targets --locked` (green), `cargo fmt --all -- --check` (green), `cargo clippy --workspace --all-targets --locked -- -D warnings` (green). | TNG-022 moved Open -> In progress. This remains a large finding -- category-store persistence, peer banning, move-on-completion, and the full method-by-method mutation matrix with stateful round-trip/restart tests are all still open (see item detail for the complete list). |
| 2026-09-02 | Focused TNG-010/011/013/026/029 remediation: wired tier-aware restore, dormant promotion/demotion, inbound routing, tiered stats, aggregate dormant restore events, and persisted tracker-deadline promotion/reannounce through a shared deadline wheel; added a bounded two-worker storage dispatcher with async pause, cancellation, durable serialized plans/checkpoints, sparse-checkpoint-safe restart recovery, and native job controls; added registry revisions, a bounded mutation journal, single-flight immutable native/qBit snapshots, lazy shared sort indexes, bounded native pagination, SSE delta/resync cursors, qBit snapshot pagination and journal-backed `sync/maindata`, stats caching/parallel task queries, aggregate transfer-rate stats, and a large-output guard against per-torrent qBit actor round trips. Fixed WebUI select-all to walk 5,000-row pages pinned to one snapshot. | Focused native/qBit/engine tests: 46/60/144 passed; full workspace tests, format, and clippy with warnings denied passed. Rebuilt the release binary with `cargo build --release --locked -p torrentngd` in 24.73 s after fixing overdue tracker deadlines. The final release binary smoke endpoints, aggregate transfer endpoints, metrics, and SIGTERM passed; SHA-256 is `c4540162a4f75b31486bf425c1f81d038a0ed0ad813fbd7f7360bf07bb736ecc`. [`local-release-backend-burndown-final-20260902.md`](../certification/reports/local-release-backend-burndown-final-20260902.md) passed implementation gates with warnings; [`external-evidence-preflight-backend-burndown-final-20260902.md`](../certification/reports/external-evidence-preflight-backend-burndown-final-20260902.md) is `PASS_WITH_WARNINGS` (3); strict and local readiness are `FAIL`. | TNG-010/011/013/026/029 remain In progress. The release certificate remains blocked: no 100k release-binary scale run, no real storage target, no public/live compatibility run, no 24h soak, no subsystem fault-injection matrix, and the full dormant representation and storage reconciliation gaps remain. |
| 2026-09-02 | Continuation checkpoint: separated production storage-worker SQLite persistence from the engine actor connection, enabled worker WAL/foreign-key settings, added a real `Engine::start` supervisor-health/shutdown test, and changed native/qBittorrent snapshot expiry to apply retained registry-journal changes before falling back to a full registry projection. Added incremental-refresh, active-job aggregate, and SSE disconnect metrics plus regression tests. | `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, full workspace tests (154 engine tests; all green), sidecar tests (76 unit and 75 integration; all green), compose config, certification policy/bundle self-tests, release scale (19/19; 100k idle RSS 155,467,776 bytes, 30 -> 14 fds, 15 -> 3 tasks), and release-binary smoke all pass. Final binary is 18,737,040 bytes, SHA-256 `e7f193a5d69ccb8bf49f74b21f8f162f051bd6130895f6b925b04bc17fee2cfc`. Local readiness is PASS; strict readiness remains FAIL because universal/live compatibility is skipped/stale, external preflight has 3 warnings, the 24h soak is stale, and post-soak evidence is old. | TNG-010/011/013/026/029 remain In progress. Registry compact replacement, live crash/failure injection, sublinear snapshot-index refresh, slow-client load evidence, production/public/device/24h evidence, and concurrent fault containment are still open. |
| 2026-09-02 | Functional-isolation continuation: bounded native SSE initial snapshots (default 500/max 1,000) now retain one revision and mark completion; engine liveness is explicit and unexpected torrent-task exits are reaped into durable error state; storage shutdown requeues active/queued jobs, payload deletion is worker-backed with idempotent completion/recovery, and durable file projections remove metainfo parsing from delete/move finalization. Move planning, native metadata/blob/webseed reads, and pure-v2 file-root rechecks now run behind detached blocking boundaries; engine task-stat collection has a 250 ms aggregate deadline; native and qBittorrent facet endpoints reuse cached snapshots. | Focused tests: `rt-session` 23, `rt-storage` 118, `rt-engine` 160, native API 48, qBittorrent API 62; focused clippy and format checks pass. | The current source checkpoint is not represented by the older `ac7fc55c...` release artifact. No 100k-hot, public, real-device, or soak proof was rerun; those extended gates remain explicitly deferred. `ensure_torrent_task` promotion parsing and the live crash/failure matrix remain open implementation seams. |
| 2026-09-02 | Functional-isolation correction: dormant-torrent promotion and magnet metadata parsing now run through detached blocking preparation; concurrent promotion actions coalesce, DHT identity inspection is detached, and qBittorrent peer logs query only promoted tasks in parallel with a deadline. | Focused source tests: `rt-engine` 161, native API 48, qBittorrent API 62; focused clippy, format, and `git diff --check` pass. | The current source remains newer than the `ac7fc55c...` release artifact. Compact dormant registry replacement, live DB/storage failure injection, and extended release/public/device/soak proof remain open/deferred. |
| 2026-09-02 | Functional-isolation integrity pass: retained the registry/DB projection until asynchronous payload deletion succeeds; added per-torrent active-job admission guards so move/delete/recheck operations cannot overlap; discarded stale detached magnet/pure-v2/promotion completions; made engine, restart, storage-plan, torrent-task, and recheck job/event writes transactional; added registry rollback on failed state/progress projections; and coalesced transfer-stat persistence instead of upserting a torrent row per block/upload notification. Added a valid paused-to-metadata-pending state transition and regression coverage for the newly exposed state-machine path. | `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --all-targets --locked` (green), sidecar tests (green), focused `rt-engine` 165/165 and `rt-session` 24/24. No release rebuild or extended scale/public/device/soak run was performed. | TNG-008/011/029 implementation evidence strengthened; TNG-010/013/026/029 remain In progress. Compact dormant replacement, migration/reconciliation work, live DB/storage failure injection, sublinear snapshot refresh, and release/public/device/soak evidence remain open/deferred. |
| 2026-09-03 UTC (prior checkpoint) | Prior source/release reconciliation: completed the compact dormant registry path, tightened Deluge/Transmission/rTorrent/qBittorrent false-success behavior, made native file/tracker reads fail closed without an engine, added durable category/tag/ban projections and peer-ban eviction, added aggregate connected-peer stats, and made TCP peer-listener failure visible to readiness while preserving control-command service. | Prior local checks and the superseded 20,420,504-byte artifact; see the final checkpoint immediately below. | Superseded by the final local verification row below. |
| 2026-09-03 UTC | Final local verification and release refresh: bounded engine command sends/replies, peer-event and socket writes, metadata completion, and persisted control-plane settings are covered; malformed compatibility inputs fail closed; qBittorrent RSS state is durable with the engine; registry rollback, task-reap error projection, transactional row/event updates, bounded persisted reads, bounded SCGI/backend responses, literal tracker matching, bounded workflow script output, bounded local configuration reads, storage command choreography, and explicit engine subsystem ownership are covered. | `cargo fmt --all -- --check`, OpenAPI validation (58 paths / 79 operations), `git diff --check`, workspace check, warnings-denied clippy, native workspace check/tests, sidecar check/tests (95 and 75 passed), declared MSRV build/tests (1.88 main, 1.97 sidecar), release build, Compose config validation, and authenticated release-binary smoke (456 ms; clean SIGTERM). Current binary: 21,235,160 bytes, SHA-256 `3c240485708a47eb0c729c4c0a7c198d357f34170fd95654a7e43be92404c3ba`; [`backend-burndown-native-release-smoke-current-20260903.md`](../certification/reports/backend-burndown-native-release-smoke-current-20260903.md). | Functional remediation is complete for the declared scopes of TNG-010/011/013/029; TNG-026 local evidence is current. Further actor decomposition, live fault/load injection, hosted CI observation, public/device/soak compatibility, and extended scale certification remain explicitly deferred. |
| 2026-09-03 UTC | Continued isolation pass: moved tracker announce/scrape transport and worker lifecycle into `crates/rt-engine/src/tracker_runtime.rs`; `TrackerWorkers` now owns the per-torrent in-flight cap, abort handles, generation fencing, response limits, and actor-drop cleanup while `TorrentTask` retains ordered state application and tier failover. Stopped announces remain actor-awaited for terminal semantics, but run in bounded parallelism under a 10-second aggregate deadline. The sidecar qBittorrent cursor now uses durable logical revisions plus bounded deletion tombstones, and single-tag add/remove projections now commit atomically with their cursor touch. | `cargo test -p rt-engine --lib --locked` (188 passed), `cargo clippy -p rt-engine --lib --locked -- -D warnings` (pass), sidecar tests (100 unit and 77 integration passed), sidecar strict clippy (pass), `git diff --check` (pass). | TNG-013/TNG-029 implementation seams strengthened. Tracker transport no longer lives in the actor module; sidecar same-second update/deletion loss and tag projection partial-write windows are closed. External load, fault-injection, hosted-CI, public/device/soak, and extended-scale proof remain deferred. |
| 2026-09-03 UTC | Rebuilt and reran the release artifact after the isolation and sidecar changes; the authenticated daemon smoke still passed health, native/qBittorrent list and transfer, Prometheus metrics, Compose rendering, and clean SIGTERM. | `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --all-targets --locked`, OpenAPI validation, sidecar tests/clippy, `cargo +1.88 test --workspace --all-targets --locked`, and `cargo +1.97.0 test --manifest-path sidecar/Cargo.toml --locked` all pass; `cargo build --release --locked -p torrentngd` passes; `target/release/torrentngd` is 21,311,008 bytes with SHA-256 `914821f9826ced3b7a2b9c4678e425ff05e9e1adf3a4b4c7682dfaa3a3611503`; [`backend-burndown-native-release-smoke-current-20260903.md`](../certification/reports/backend-burndown-native-release-smoke-current-20260903.md) (pass, 464 ms, two shutdown polls). | TNG-026 local release evidence is current again. This remains local deployment smoke, not public/device/24-hour, universal-compatibility, or capacity certification. |
| 2026-09-03 local / 2026-09-04 UTC | Continued the isolation burn-down: native sidebar media facets now use an incremental snapshot index; the production engine session-event path uses a bounded ordered SQLite writer with drain-on-shutdown; session-log reads and engine operator read commands (trackers, settings, categories/tags, jobs, storage roots, global/network settings, and queue priority) execute in detached blocking workers instead of directly in the actor. Added regression coverage for incremental media-facet updates. | `cargo fmt --all -- --check`, `git diff --check`, `cargo check --workspace --locked`, full workspace tests, full workspace warnings-denied clippy, sidecar tests (121 unit / 83 integration), sidecar warnings-denied clippy, main MSRV tests on Rust 1.88, sidecar MSRV tests on Rust 1.97.0, OpenAPI validation (58 paths / 79 operations), release build, Compose validation, and authenticated release smoke (474 ms; clean SIGTERM) all pass. Current artifact: 21,993,112 bytes, SHA-256 `1d5fe1bee668179001dab21ac697aea01bb0f2cb11276f13208c38975cacd28e`; [`backend-burndown-native-release-smoke-current-20260903.md`](../certification/reports/backend-burndown-native-release-smoke-current-20260903.md). | TNG-013 implementation tightened: native facet scans and operator read stalls reduced; TNG-026 local evidence refreshed. TNG-029 is explicitly still partial: authoritative actor-side torrent/job persistence and deeper dependency extraction remain; live fault/load, hosted-CI, public/device/soak, and extended-scale proof remain deferred. |
| 2026-09-03 local / 2026-09-04 UTC | Closed the stated TNG-029 persistence seam: the production `Engine` no longer retains a shared SQLite mutex; authoritative torrent/job/state reads and writes use a bounded ordered `DbWorker` with a private connection, cancellation fencing, panic containment, health reporting, and drain-on-shutdown. Production storage submissions use the storage supervisor's private checkpoint connection; direct database helpers are test-only. Added an external rTorrent library-entry-point contract test and explicit pure-v2 boundary documentation. | `cargo test -p rt-engine --locked` (200 passed), `cargo test -p rt-api-rtorrent --locked` (26 unit + 2 external integration passed), `cargo check -p rt-engine --release --locked`, deterministic strict fault matrix (pass), live release fault matrix (SIGKILL/restart, API cancellation, SQLite failure/recovery, filesystem failure; pass), API/SSE load (92,000 requests, 32 JSON clients, 8 slow SSE consumers, zero errors; pass), final release smoke (22,403,824 bytes, SHA-256 `1d1ca3b5528c77f51aa3dff5a2e090e82d5ac7bd3de932e98b172bfb67b121d4`, 462 ms; pass). | TNG-011/TNG-013/TNG-029 local implementation and evidence gates are closed for their declared scopes. TNG-006's library boundary is explicit and tested; pure-v2 transfer/completion remains intentionally unsupported. Hosted CI observation, public compatibility, real-device storage, 24-hour soak, representative production-corpus allocation evidence, and extended capacity proof remain external/deferred. |

| 2026-09-03 local / 2026-09-04 UTC | Rebuilt the current release artifact and reran the strict local fault matrix, live daemon fault matrix, 30-second API/SSE load, and local Docker interoperability matrix. Corrected a real peer self-connection fallback that disabled webseed recovery, corrected qBittorrent progress/piece projection after stale completion timestamps, and fixed the interop harness to assert documented 501 responses for unsupported qBittorrent mutations. | Release smoke: 22,470,824 bytes, SHA-256 `caa3c725bdd29e49677dfd0bf11a70904650d5954092f533e1684ceab7fd1f76`, 466 ms; fault matrix PASS; 191,893 requests with 32 JSON clients and 8 slow SSE consumers, zero errors; local interop [`interop-matrix-backend-local-20260904-final.md`](../certification/reports/interop-matrix-backend-local-20260904-final.md) reconciles 28/28 PASS. | TNG-011/TNG-013/TNG-026/TNG-029 local implementation and evidence gates remain closed for their declared scopes. Pure-v2 transfer/completion is explicitly unsupported and the rTorrent library boundary is documented/tested. Hosted CI observation, public Internet compatibility, real-device storage, 24-hour soak, allocator/production-corpus evidence, and extended capacity proof remain external or deferred. |
| 2026-09-04 local / 2026-09-04 UTC | Final local burn-down refresh: rebuilt the release binary after metrics privacy and sidecar security changes; completed the live storage/DB fault matrix, many-client/slow-SSE load, storage regression suite, webseed deadline scheduler, peer-channel budget, sidecar auth/default-bind/proxy tests, security scan, policy self-tests, and universal-live local Docker interop. Default Prometheus labels hash torrent identifiers; raw IDs require explicit opt-in. | Release smoke [`backend-burndown-native-release-smoke-current-20260904.md`](../certification/reports/backend-burndown-native-release-smoke-current-20260904.md): 22,518,096 bytes, SHA-256 `9f2dd59ba4bff2f760c789288dc057aab22c0f957ce4e47c36d51f0ff6699288`, 470 ms; sidecar 125 unit/87 integration tests; rt-storage 130 tests; security scan PASS with no HIGH/CRITICAL image findings; universal-live [`universal-live-backend-local-20260904-current-pass.md`](../certification/reports/universal-live-backend-local-20260904-current-pass.md) PASS_WITH_SKIPS with 28/28 local cases PASS. | All repository-actionable TNG implementation, contract, security, CI, and local-evidence work is closed for the declared scope. Remaining gates are external-only: hosted CI observation/branch protection, public-client/network interop, target-device storage, 24-hour soak, production-corpus allocator/fairness/transport profiles, and optional 100k capacity proof. Pure-v2 transfer/completion and the unowned rTorrent HTTP-server interpretation remain explicitly unsupported. |
| 2026-09-04 local / 2026-09-04 UTC | CI failure burn-down and evidence refresh: fixed hosted scheduling/fixture races, made recovery evidence portable, made certification archives pipefail-safe, made missing security tooling fail closed, waited for daemon readiness after restart, fenced cleanup assertions on durable DB state, and removed the interop metrics probe SIGPIPE. Rebuilt the clean release artifact and reran the current full Docker matrix. | Hosted CI run `33915548520` is green with all 10 jobs on `f1c39fd`; dynamic CodeQL run `33915547352` is green with all four analyses; release artifact is 22,433,352 bytes with SHA-256 `ff94ede075f7541ef9eecf5418b1c31324fb1b6ca2648681d975b3e9cd048e73` and 462 ms smoke; current local Docker matrix [`interop-matrix-20260904T195529Z.md`](../certification/reports/interop-matrix-20260904T195529Z.md) is 28/28 PASS. | TNG-025/027/028 repository gates are resolved. Branch-protection enforcement, public/client/device/24-hour evidence, production-corpus profiling, and optional extended-capacity proof remain explicit external or optional gates. |
| 2026-09-05 local / 2026-09-05 UTC | Ran the official Debian 13.6 netinst public torrent through the Docker interop stack. Corrected public-mode client setup to use the host-resolved metainfo file, made interop DNS overrideable, added exact name/hash/completed-state assertions to the soak runner, corrected thread telemetry to read Linux `Threads:`, and made the 24-hour launcher prefer a supervised user-systemd unit. | [`public-debian-interop-20260905T191253Z.md`](../certification/reports/public-debian-interop-20260905T191253Z.md) is PASS: 791,674,880 bytes, Rust complete, three reference-client peers. The named 86,400-second soak is active under `torrentng-public-debian-soak-20260905.service`; launch details are in [`PUBLIC_TORRENT_SOAK_20260905.md`](PUBLIC_TORRENT_SOAK_20260905.md). Initial post-launch samples are healthy; final soak status is not yet proven. | One public-swarm transfer is now evidenced. Remaining external gates are the completed long soak, real-device storage, remaining approved public sources/universal compatibility, and optional capacity/profiling work. |

## Release gate

The native release gate must fail while any P0 item is Open or while TNG-025,
TNG-026, or TNG-028 is Open. A production-scale claim additionally requires
TNG-010, TNG-013, and TNG-014 to be Resolved with release-artifact evidence.
