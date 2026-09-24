# TorrentNG Backend Audit Burn-down

Status: **implementation burn-down continuing; local source validation refreshed 2026-09-21**
Baseline: 2026-09-01, `main`  
Scope: the TorrentNG client (`torrentngd`), the compatible-client WebUI/API
service (`torrentng`), their API facades, storage, deployment, CI, and release
evidence.

This is the canonical remediation ledger for the principal-engineer / investor
audit. Older roadmap and certification documents describe intended or locally
tested behavior; they are not proof that a feature is wired into the live
runtime. An item is not complete until its code path, focused regression test,
and release evidence exist together.

## Executive decision

TorrentNG is not certified as a production-grade 100k-torrent deployment or as
a universally compatible client. The current source has materially closed the
functional storage, lifecycle, snapshot, and compatibility gaps, and the
release binary passes the local authenticated daemon smoke. One official public
Debian transfer, its completed counted soak, and the current kspls0 LVM storage
qualification now have passing evidence. The release posture remains **do not
make unqualified scale, security, public-interoperability, or universal-compatibility claims**
until the remaining evidence exists.

The burn-down order is:

1. security and data integrity;
2. runtime limits, lifecycle, and scale;
3. API and compatibility truth;
4. deployment, CI, and independent release evidence;
5. architecture seams and maintainability.

Current execution priority is functional correctness and isolation. The
100k-hot, broader public-compatibility, production-corpus, and multi-device
qualification gates are extended proof work, not the current implementation
gate; they remain explicitly bounded and must not be represented as completed
by local unit tests or a synthetic dormant corpus.

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

## Current source validation (2026-09-21)

This is verification of the in-progress source tree, not a refreshed release
artifact or external qualification claim.

This refresh reran the workspace tests and warnings-denied clippy, sidecar
tests/clippy/formatting, WebUI tests/lint/build, repository-wide
`shellcheck -S warning`, shell syntax, dependency audits, and the container security scan. Cross-platform/Wine and
local certification self-tests below retain their previously recorded
2026-09-20 evidence unless stated otherwise.

Dedicated torrent-count/capacity proofs and long soaks are out of scope for
this bug/security pass. The ordinary workspace suite includes synthetic
`rt-metrics` scale regressions; those are not release-capacity evidence. The
dedicated capacity runner and soak are not being run or used to hold these
implementation fixes open. Revisit soak evidence during the later test phase,
and make no unverified deployment-capacity claim.

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | Workspace tests pass on the current source tree. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | Main-workspace warnings-denied lint passes. |
| Native API/model targeted tests and clippy | PASS | The current API/model suites pass in `rt-api-model` (21 tests), `rt-api-native` (99), and `rt-api-qbit` (91); all three pass warnings-denied clippy. |
| Sidecar tests / clippy / formatting | PASS | 242 library, 3 binary, and 112 compatibility tests pass; Linux warnings-denied Clippy and formatting pass. Two benchmark cases remain ignored. The current sidecar test target cross-compiles for Windows GNU. The OpenAPI contract validates at 59 paths / 80 operations. |
| WebUI tests / typecheck / lint / production build | PASS | All 7 WebUI tests pass; `tsc --noEmit`, ESLint, and the production build pass. Build output was directed to a fresh temporary directory rather than replacing bundled static assets. |
| Sidecar HTTP error redaction | PASS | Production backend send, HTTP-status, and bounded response-body errors discard reqwest request URLs; refused-connection and HTTP-500 regressions verify query secrets are absent from the full error chain. |
| Tracker URL diagnostic redaction | PASS | Sidecar diagnostics retain only the URL origin; credentials, path-embedded passkeys, queries, fragments, magnets, and malformed URLs are masked. Tracker mutation failures do not reflect raw URLs or backend error text into API responses/logs. |
| rTorrent log URL redaction | PASS | Persisted operator-log lines redact embedded scheme-qualified URLs to their origin, mask embedded magnets, and escape terminal/Unicode line/bidi controls; regressions cover credential-bearing URL shapes and persisted control characters. |
| Torrent-ingestion error diagnostics | PASS | Add-magnet, add-URL, metainfo, and RSS-magnet failures no longer log or return arbitrary backend error text that could echo submitted tracker credentials. URL/magnet source labels are redacted. |
| Tracker response message redaction | PASS | Untrusted tracker warning/failure messages redact embedded URLs, magnets, and common credential-like query fields before response logging or state persistence; the tracker-state boundary repeats sanitization for direct callers. |
| WebUI tracker URL labels | PASS | Tracker displays retain only a validated HTTP(S)/UDP origin; all userinfo, paths, queries, and fragments are hidden unless the user explicitly reveals or copies the URL. Unsupported and malformed values fail closed. |
| Session-cookie transport flags | PASS | Login and logout use `Secure` by default; only loopback listeners may opt out, and both paths have integration regressions. |
| `cargo test --locked -p rt-storage --lib` | PASS | 199 storage unit tests pass; hardware and real-device suites remain ignored. |
| Poisoned-mutex recovery regressions | PASS | API sort, egress, storage cache/pool, worker queue, admission/rate budget, idempotency, prepared-file, and shutdown-handle recovery tests pass; disposable state is rebuilt while authoritative state is retained. |
| Storage/native API test-target cross-checks | PASS | `rt-storage --tests` and `rt-api-native --tests` compile for Windows GNU and FreeBSD; `rt-storage --tests` also compiles for macOS. The combined macOS native API cross-check is unavailable with the installed host C compiler because it cannot build `ring`/SQLite for Apple target flags. |
| Windows storage tests under Wine | PARTIAL | 156 of 158 pass, including all 56 plan tests. Two unlink/recreate identity tests fail while Wine logs an unimplemented `SetFileInformationByHandle` deletion-disposition class; this is not native Windows runtime evidence. |
| Windows path/metainfo validation under Wine | PASS | `rt-path` passes 17 tests and `rt-metainfo` passes 61; v1/v2 parsing rejects alternate streams and Win32 path aliases. |
| Sidecar filesystem authority | PASS | Identity/metainfo reads reject final symlink/reparse aliases; config, stats, logs, and overlays verify the opened handle is a regular file, using nonblocking Unix opens so FIFOs cannot hang readers. Peer-ID repair atomically replaces a planted link without changing its target. |
| Hybrid layout and BEP 47 padding | PASS | Parser and engine reject inconsistent v1/v2 payload order, paths, lengths, or piece alignment; synthetic padding is zero-validated and never materialized as a file. |
| Private torrent flag parsing | PASS | Missing/zero remains public, `private=1` remains private, and a present non-integer or unknown integer is rejected before DHT/PEX policy can consume it. |
| Private tracker lifecycle | PASS | Private v1/hybrid and pure-v2 torrents announce to one configured tracker at a time, fail over in order, and discard old-tracker peer state on failover or tracker-list replacement. |
| Tracker URL log redaction | PASS | Tracker log labels omit credentials, path, query, and fragment; HTTP transport errors omit reqwest's URL. |
| Task panic-payload redaction | PASS | Explicit Tokio `JoinError` conversions across engine workers, storage jobs, compatibility projections, CLI commands, and fastresume expose only static task/outcome summaries; 444 `rt-engine` and 29 `rt-fastresume` tests pass. |
| Daemon panic-hook output | PASS | The daemon installs a location-only panic hook before argument/config handling; an isolated subprocess verifies stderr contains the source location but not the panic canary. |
| Task panic-payload redaction | PASS | Explicit Tokio `JoinError` conversions across engine workers, storage jobs, compatibility projections, CLI commands, and fastresume expose only static task/outcome summaries; 444 `rt-engine` and 29 `rt-fastresume` tests pass. |
| Default metainfo convenience allocation bound | PASS | `parse_torrent`, `torrent_info_bytes`, and `v2_piece_layer_requirements` use a cumulative 512 MiB per-call reservation cap and reject input over 64 MiB. The callback APIs remain caller-governed. |
| Parser allocator/RSS qualification | GAP | Engine-admitted paths reserve decoder growth, derived projections, and raw copies before allocation; default convenience APIs also enforce a cumulative 512 MiB per-call cap. Aggregate concurrency and actual allocator/RSS behavior remain unmeasured. |
| OpenAPI and local certification self-tests | PASS | OpenAPI validates 59 paths / 80 operations; policy, bundle, and storage self-tests pass. |
| Curl certification wrapper | PASS | Authenticated output accepts only status/timing write-outs; response-header dumps and arbitrary/file-backed write-out formats fail closed. Inline `--json` and Referer values are spooled privately; GET JSON fields are inspected; cookie and netrc input/output files are owner-checked and mode `0600`; automatic Referer redirect mode is refused; FIFO and symlink header-file sources fail closed. |
| Python policy/protected-target/API-load regressions | PASS | All 39 tests pass, including fake-curl secret-in-argv, cookie-jar, redirect, GET-query, JSON-body, response-header output, proxy-header, special-header-file, and management-port checks. |
| WebUI dependency audit | PASS | `npm audit --audit-level=high` reports zero known vulnerabilities. |
| RustSec audit | PASS | All three lockfiles have zero known vulnerabilities or yanked-package warnings. |
| Other security checks | PASS | `shellcheck -S warning` across all repository shell scripts, Bash/POSIX-shell syntax checks, Compose hardening and custom Phase 1 TCP/UDP port mapping, all three RustSec lockfiles, and Trivy image/config scans pass; no HIGH/CRITICAL findings were reported. |

Hybrid metadata now has one shared validator for the parser and engine: it
checks that both views contain the same non-padding payload in the same order,
with matching paths and lengths and v1 padding at the v2 piece boundaries.
Rooted and rootless v2 trees are accepted, as is either an unpadded logical
end or a correctly aligned v1 tail pad. For v1 BEP 47 padding, piece hashes
still cover the zero bytes, but storage and file projections do not create a
`.pad` file; uploads and verification synthesize zeros and non-zero peer pad
bytes are rejected. The implementation follows [BEP 52](https://www.bittorrent.org/beps/bep_0052.html)
and [BEP 47](https://www.bittorrent.org/beps/bep_0047.html).

Windows runtime storage opens and portable plan operations reject reparse
points, and portable copies/deletes honor control checks during long steps.
The Windows planner uses atomic no-replace moves, no-follow source/hash opens,
exclusive no-follow destination creation, and copy-only imports instead of
path-based hard-link creation. Windows documents that hard-link creation
follows a symbolic-link target ([Microsoft reference](https://learn.microsoft.com/en-us/windows/win32/fileio/symbolic-link-effects-on-file-systems-functions)).
Recursive operations now retain ancestor and active-directory handles opened
without delete-sharing, preventing directory replacement while those
path-based traversals are in progress. The executor is still not fully
handle-relative; exact native Windows race behavior remains unqualified, and
the remaining gap is explicit below rather than treated as Unix parity.

Windows-compatible torrent paths now reject alternate-stream syntax, invalid
Win32 characters, reserved device names (including superscript COM/LPT aliases),
and names whose leading/trailing ASCII spaces or trailing periods are
normalized by Windows. Both v1 and v2 metainfo parsing apply these checks on
Windows. Metainfo parsing also rejects duplicate/file-directory-conflicting
paths, limits each torrent path to 64 components to match storage-plan
traversal, and caps expanded path ownership at 500,000 components and 16 MiB.
The aggregate cap prevents shared BEP 52 prefixes from being copied without
bound for every leaf file. These rules follow Microsoft's [file-naming guidance](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file) and [alternate-data-stream rules](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams).

The sidecar's shared bounded-file reader now opens the final Windows path
component with `FILE_FLAG_OPEN_REPARSE_POINT` and rejects reparse-point
attributes from the opened handle, matching its Unix `O_NOFOLLOW` behavior.
Both rTorrent session-metainfo reads and peer-ID suffix reads use it. Invalid
peer-ID state is repaired through a unique same-directory temporary file and
rename, so a planted symlink is replaced as a link rather than followed to
overwrite its target. Unix and Wine regressions cover both paths; native
Windows execution is still not claimed.

The RustSec CI jobs now audit all three checked-in lockfiles (`Cargo.lock`,
`sidecar/Cargo.lock`, and `fuzz/Cargo.lock`). Local RustSec auditing also found
and closed a sidecar TLS dependency advisory: `rustls 0.23.44` was upgraded to
patched `0.23.45` for
[RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285). The
root lockfile's yanked `spin 0.9.8` warning was cleared by upgrading to
`0.9.9`. All three lockfiles now audit without vulnerabilities or yanked
warnings. Container scanning remains unverified locally because neither Trivy
nor the `torrentng:certification` image is available.

## Baseline evidence

The following was run against the audit baseline before this burn-down began:

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | Existing TorrentNG-client tests are green, but mostly exercise isolated behavior. |
| `cargo test --manifest-path sidecar/Cargo.toml --locked` | PASS | Compatible-client service tests are green. |
| `cargo fmt --all -- --check` | FAIL | `crates/rt-migrate/src/lib.rs:2663` was not formatted. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | FAIL | Existing lint/MSRV/enum-layout failures remain. |
| TorrentNG-client CI workflow | INCOMPLETE | `.github/workflows/ci.yml` builds the compatible-client service/WebUI but does not test TorrentNG-client crates. |
| TorrentNG-client release workflow (baseline) | INCOMPLETE | Historical baseline: release built and smoke-checked the binary without TorrentNG-client test, fmt, or clippy gates. |
| certification status | NOT CLEAN | Universal compatibility is `PASS_WITH_SKIPS` because the separate real-device wrapper leg is intentionally skipped; the completed 24h soak is PASS, while strict readiness still fails on non-clean evidence rows. |
| checked-in fuzz/OpenAPI/idempotency evidence | PARTIAL | Fuzz targets and bounded CI smoke commands are checked in; the TorrentNG API OpenAPI contract is now checked in; endpoint replay tests and an observed hosted-CI run remain evidence gaps. |

## Prior release and qualification evidence (2026-09-10 local / 2026-09-10 UTC)

The current source tree has been re-verified after the functional isolation,
durability, compatibility, and deployment fixes recorded in the latest
burn-down entries. Runtime and external qualification evidence below targets
product commit `b393eb0`; evidence reconciliation is `3cb0ba4`, and the latest
certification-script hardening is `50e0fc3`. The public Debian transfer was
rerun, the kspls0 LV was exercised directly, and the external preflight was
rerun in strict mode. The counted public soak source predates b393; its
finalization report is explicit about the source report and does not claim a
b393 release soak.

| Check | Result | Meaning |
| --- | --- | --- |
| `cargo test --workspace --all-targets --locked` | PASS | TorrentNG-client workspace tests green, with only explicitly ignored real-device tests skipped. |
| `cargo test --manifest-path sidecar/Cargo.toml --locked` | PASS | Compatible-client service tests green; two synthetic benchmarks remain explicitly ignored. |
| `cargo fmt --all -- --check` | PASS | Workspace formatting is clean. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS | Warnings-denied lint is clean. |
| `cargo +1.88 build/test --workspace --all-targets --locked` | PASS | Declared main-workspace MSRV build and tests green. |
| `cargo +1.97.0 build/test --manifest-path sidecar/Cargo.toml --locked` | PASS | Declared compatible-client service MSRV build and tests green. |
| declared `rust-version` (both `Cargo.toml`s) | **CORRECTED** | Was `1.80` in both, unverified and untrue. Neither workspace's *locked* dependency graph builds below 1.88 (main: `idna_adapter` needs rustc 1.86+, plus `edition2024` needs Cargo 1.85+) or 1.97 (sidecar: `libsqlite3-sys`'s build script uses `cfg_select!`, stabilized between 1.94 and 1.97). This is a transitive-dependency floor, not first-party code needing new syntax. Corrected both `rust-version` fields to `1.88` / `1.97` to match reality; this itself is TNG-028 acceptance criteria ("document the supported toolchain"). |
| GitHub Actions CI run `34521941751` | PASS | All 10 jobs passed on `196c65a`: TorrentNG-client quality, both MSRV jobs, fuzz smoke, compatible-client service, WebUI, dependency security, backup/restore, API/SSE load, and fault containment. |
| rTorrent startup identity timeout isolation | PASS | Commit `4a90048` gives the multi-thousand-download identity rewrite a separate 300-second timeout while ordinary XMLRPC calls remain 10 seconds; compatible-client service tests and warnings-denied clippy pass. |
| kspls0 LVM storage release certification | PASS | [`storage-release-certification-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/storage-release-certification-kspls0-lvm-20260910-b393eb0.md); exact commit `b393eb0`, `/dev/mapper/datapool_lvm-media`, HDD median ratio 5.11x, io_uring graduation, and real-root move/import all pass. |
| kspls0 real-device storage matrix | PASS_WITH_SKIPS | [`universal-live-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/universal-live-kspls0-lvm-20260910-b393eb0.md); the real-device storage gate passes against the LV; local Docker/public legs were intentionally not rerun in this targeted invocation. |
| Public Debian 24-hour soak finalization | PASS | [`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md); 1,437 samples, one exact completed torrent, resource/health checks pass. |
| Canonical all-live compatibility certification | PASS_WITH_SKIPS | [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md); static, migration, local Docker, mobile, and public Debian gates pass; only the separate real-device wrapper gate is skipped. |
| Clean release-binary smoke | PASS | [`backend-burndown-native-release-smoke-20260910-final.md`](../certification/reports/backend-burndown-native-release-smoke-20260910-final.md); build commit `3cb0ba4`, 22,449,216 bytes, SHA-256 `7fbac478b696316d989028c47573e4cd248f04a98a479f218017c0ec5a812b5e`, 457 ms, clean SIGTERM. |
| Full local release gate | PASS_WITH_WARNINGS | [`local-release-20260910-50e0fc3.md`](../certification/reports/local-release-20260910-50e0fc3.md); TorrentNG-client, storage-feature, WebUI, API, smoke, backup, corpus, and security gates pass; only local block-device certification is skipped. |
| Strict external evidence preflight | PASS | [`external-evidence-preflight-release-strict-20260910-b393eb0.md`](../certification/reports/external-evidence-preflight-release-strict-20260910-b393eb0.md); Docker, public opt-in, writable target, migration corpus, and completed soak pass. |

The hosted CI workflow has now run successfully for the pushed source. Run
`34521941751` passed all ten jobs and the companion dynamic CodeQL orchestration
run `34521941269` passed all four analyses on `196c65a`. This proves the
repository gates execute on GitHub's runners; it does not prove that branch
protection requires them, and the repository's branch-protection setting must
still be reviewed separately.

Focused release evidence from 2026-09-04 is indexed in
[`BACKEND_BURNDOWN_RELEASE_20260902.md`](BACKEND_BURNDOWN_RELEASE_20260902.md).
The current clean release binary was built at `3cb0ba4`, launched with an
isolated authenticated config, exercised through TorrentNG REST, qBittorrent REST,
health, and metrics, and terminated with SIGTERM. The full local release gate
was rerun at `50e0fc3` and is `PASS_WITH_WARNINGS` solely because its local
block-device target was not configured. Strict readiness remains appropriately
blocked by explicit compatibility/storage-scope policy rows. This is a
deployment smoke result, not a 100k capacity result.

The final clean release-binary smoke report is
[`backend-burndown-native-release-smoke-20260910-final.md`](../certification/reports/backend-burndown-native-release-smoke-20260910-final.md):
22,449,216 bytes, SHA-256
`7fbac478b696316d989028c47573e4cd248f04a98a479f218017c0ec5a812b5e`, 457 ms,
all checks PASS, and clean SIGTERM. The latest WebUI report is
[`webui-certification-20260910-50e0fc3.md`](../certification/reports/webui-certification-20260910-50e0fc3.md),
and the latest full local release report is
[`local-release-20260910-50e0fc3.md`](../certification/reports/local-release-20260910-50e0fc3.md).
Full workspace tests, warnings-denied clippy, formatting, OpenAPI validation,
compatible-client service tests, the current security scan, and the universal-live local Docker
matrix are green. These are current local facts, not external production
evidence. The current b393 all-live compatibility report is `PASS_WITH_SKIPS`:
its local Docker, mobile, and public legs pass, while the real-device wrapper
is explicitly skipped in
[`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md).
The separate kspls0 LVM storage qualification also passes; its targeted
universal-live report records the real-device storage gate as PASS and leaves
the unrelated local/public legs explicitly skipped.

The evidence reconciliation is `3cb0ba4`; certification-harness hardening is
`50e0fc3` and does not change the daemon binary. The local release process also
passed the focused fault and API-load gates:
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
[`interop-matrix-20260910T190228Z.md`](../certification/reports/interop-matrix-20260910T190228Z.md).
It covers bidirectional transfers with qBittorrent, Transmission, Deluge, and
rTorrent, failure/recovery protocol cases, and TorrentNG/qBittorrent/Transmission/
Deluge facade mutations.

The current live public-torrent matrix is
[`interop-matrix-20260910T192200Z.md`](../certification/reports/interop-matrix-20260910T192200Z.md),
with the canonical all-live evidence in
[`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md).
It resolved the official Debian 13.6 netinst torrent, supplied its verified
metainfo to Rust and the four reference clients, transferred 791,674,880
bytes, reached 100% in Rust, and observed 142 Rust peers across all five
configured clients. The latest all-live run also exercised the mobile
qBittorrent-compatible read flow and passed it. The
v1 info hash is `481b6e3617be4c88f96cb25e47c9d8272130071e`. This closes one
public-swarm evidence row; it does not establish universal compatibility.

The named public-torrent 24-hour soak is complete under the launch record
[`PUBLIC_TORRENT_SOAK_20260905.md`](PUBLIC_TORRENT_SOAK_20260905.md). The
counted source report retained 1,437 samples and the finalizer reports PASS.

The current strict external preflight is
[`external-evidence-preflight-release-strict-20260910-b393eb0.md`](../certification/reports/external-evidence-preflight-release-strict-20260910-b393eb0.md):
Docker, public opt-in, writable target, migration corpus, and completed soak
are green with no warnings.

### Pure-v2 metadata-completion evidence (2026-09-15 local)

The bounded native `btmh` path is locally covered, but this is synthetic
protocol evidence rather than public-client certification. The focused package
run passed 413 `rt-engine` tests, 53 `rt-metainfo` tests, and 7 `rt-metrics`
tests. It includes a trackerless TCP direct-peer exchange that sends the exact
BEP 9 `info` dictionary, requests a BEP 52 piece layer, authenticates its
Merkle proof, emits `CompleteMagnet`, and parses the promoted pure-v2
metainfo. Parser tests cover IPv4/IPv6 `x.pe` deduplication and limits, exact
v2 info extraction, and the no-top-level-layer preflight view. Engine tests
cover durable pending rows, restart restoration of the metadata worker, and
promotion into the normal v2 runtime. Public-client, target-device, hostile
network, and long-duration evidence remain separate open gates.

### Historical functional isolation checkpoint (2026-09-02)

This checkpoint predates the current release artifact recorded above. It is
retained to show the sequence of the remediation work; the current source and
release smoke supersede its artifact statement. The extended release/hot-set
proof gate remains deliberately deferred. The source then had
focused green coverage for the substantive seams: `rt-session` 24 tests,
`rt-storage` 118 tests, `rt-engine` 165 tests, TorrentNG API 48 tests, and
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
changes. TorrentNG facet aggregates and
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
- Idempotency-key claim/replay/conflict handling is shared by TorrentNG,
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
  reports the TorrentNG Label and Notifications surfaces.
- Migration/schema startup work is transactional, persisted projections are
  reconciled, and the TorrentNG API OpenAPI contract is checked in with a standard
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
- TNG-016's local pure-v2 transfer, tracker lifecycle, and `btmh` magnet
  metadata-completion implementation is present with bounded BEP 9/BEP 52
  exchange and proof validation. Public-network interoperability and broader
  transfer evidence are still external gates.
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
clippy, OpenAPI validation, compatible-client service tests, release build, authenticated
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
| TNG-011 | Implemented and locally fault-tested: detached bounded storage workers, dedicated DB connection, durable restart recovery, live cancellation, DB-failure, and filesystem-failure isolation; poisoned worker-DB locks now return an error instead of triggering a second panic during recovery | Hosted/device deployment evidence and broader disk/permission/space matrix remain external |
| TNG-012 | Implemented locally: actor liveness, task reaping, bounded shutdown; live dependency health and SIGTERM paths pass | Shutdown-under-load and deployment timing evidence remain external |
| TNG-013 | Implemented and locally load-tested: immutable snapshots, indexes, pagination, journals, bounded SSE chunks; poisoned derived session snapshot caches recover for rebuild instead of panicking; 204,936-request many-client/slow-consumer run passes with zero errors | Representative production corpus, allocator profile, and public/client load evidence remain external |
| TNG-014 | Implemented locally: packed bitmaps, shared immutable piece maps, and governor-bounded peer/piece state | Run peer-count and large-piece-count memory profile |
| TNG-015 | Implemented locally: guarded webseed timer and exponential retry backoff | Run idle/large-swarm benchmark |
| TNG-016 | Implemented locally: pure-v2 file-root recheck, BEP 9 `btmh` completion, BEP 52 TCP/uTP transfer, piece-layer proof validation, and tracker lifecycle | Public pure-v2 interoperability and broader transfer evidence remain open |
| TNG-017 | Implemented locally: independent rate windows and choker inputs | Run controlled transfer proof |
| TNG-018 | Implemented locally: handshake, idle, request, and response budgets | Run scheduler-saturation evidence |
| TNG-019 | Implemented locally for IPv4 and IPv6 live-DHT routing: bounds, source checks, tokens, caps, and stale-query pruning | Run hostile-input/load and restart evidence |
| TNG-020 | Implemented locally: checked tracker values, bounded UDP handling, PEX add/drop parsing and handling | Run broad tracker/transport interoperability evidence |
| TNG-021 | Resolved: TorrentNG list contract and bounded pagination agree | None beyond regression maintenance |
| TNG-022 | Implemented locally: durable categories/tags/bans and ban eviction; unsupported mode/plugin operations now fail explicitly | Keep projection-only compatibility behavior documented; run real-client matrix |
| TNG-023 | Implemented locally: implemented/enabled/certified/experimental assurance states are separate | Keep `certified` empty until external evidence is accepted |
| TNG-024 | Implemented locally: fail-closed config validation, secret-file support, separate credential-required backend overlays, authenticated Transmission wiring, loopback-only backend management ports, and rootless compatible-client/Phase 1 images; disposable legacy-volume migration and custom-UID volume writes pass | Production volume/host-mount ownership and qBittorrent/Deluge target credentials/secrets still require deployment-specific verification |
| TNG-025 | Resolved for the repository gate: TorrentNG-client and compatible-client service quality, clippy, MSRV, fuzz, release-smoke, security, backup, load, and fault jobs execute successfully | Branch-protection enforcement still needs repository-settings review |
| TNG-026 | Runtime source is `b393eb0`; release evidence was reconciled at `3cb0ba4` and the certification harness was hardened at `50e0fc3`; clean deployment smoke, backup/restore, WebUI, and shutdown now pass | One official public Debian transfer, completed named soak, canonical all-live local/mobile/public compatibility, and kspls0 LVM storage now pass; remaining public sources and strict readiness remain external gates |
| TNG-027 | Resolved for the repository gate: fuzz targets, OpenAPI validator, idempotency tests, and hosted bounded fuzz smoke are green | Broader parser and mutation replay corpus remains optional evidence work |
| TNG-028 | Resolved for the repository gate: format, clippy, locked tests, and declared MSRV pass locally and in hosted CI | Branch-protection enforcement still needs repository-settings review |
| TNG-029 | Resolved for the stated persistence-isolation finding: authoritative engine DB work uses a dedicated bounded supervised worker; live crash/DB/storage fault matrix and local client matrix pass | Full actor decomposition and deployment-specific fault evidence remain non-release structural follow-up |
| TNG-030 | Resolved locally: sidecar APIs canonicalize cache identities, native/qBittorrent selections and peer-address lists are bounded, URL dot hashes cannot normalize into other endpoints, and no-follow file reads do not block on FIFOs | Public-client, network, and soak evidence remain separate qualification work |
| TNG-031 | Resolved locally: qBittorrent delimited inputs and transient plugin/job state are bounded; RSS items/rules persist in SQLite with transactional updates and shared rule capacity | Search execution remains intentionally inert; public-network and soak evidence remain separate |
| TNG-032 | Resolved locally: root and sidecar category/tag mutation inputs now bound names, category paths, and tag arrays before persistence/backend work | Sidecar-wide dictionary and read-projection bounds are tracked in TNG-033; external-client interoperability remains separate |
| TNG-033 | Resolved locally: sidecar category/tag dictionaries, per-torrent and aggregate tag assignments, backend tag parsing, and label read projections are bounded; backend mutations preflight cache capacity | Legacy over-limit list/delta projections fail closed; backend/client interoperability and torrent-count/soak evidence remain separate |
| TNG-034 | Resolved locally: native JSON batches, live-hash queries, RSS sample strings, ratio-group matches, and workflow matches are bounded; qB `hashes=all` uses a 10,000-target lookahead cap and nested mutation fan-out is capped before backend calls | Public-client interoperability remains separate; torrent-count and soak evidence stay deferred |
| TNG-035 | Resolved locally: persistent ratio-group and workflow rule sets now cap entry count and field sizes, preserve updates at capacity, and reject new entries without growing their JSON-backed stores | Public-client interoperability and soak evidence remain separate |
| TNG-036 | Resolved locally: poisoned cache and coordination mutexes no longer cascade into repeat panics; derived projections rebuild, while rate/admission, idempotency, dirty-write, queued-job, and shutdown ownership state is preserved | Keep panic-injection regressions for these invariants; no external qualification is implied |
| TNG-037 | Resolved locally: cookie-authenticated WebSocket upgrades and browser mutations enforce same-origin/CSRF metadata, including loopback no-token mode | Keep focused origin and WebSocket regressions; no external qualification is implied |
| TNG-038 | Resolved locally: torrent-add requests reject ambiguous URL/file or magnet/file sources before backend work | Keep native, sidecar, and qB facade regressions; no external qualification is implied |
| TNG-039 | Resolved locally: WebUI handles `resync_required` by invalidating all cached query state and has a hook regression | Keep the resync invalidation test |
| TNG-040 | Resolved locally: live-speed freshness rejects timestamps more than five seconds in the future | Keep the timestamp-boundary regression |
| TNG-041 | Resolved locally: sync projections validate per-field byte limits and preserve prior cache state when a row is invalid | Keep invalid-row and stale-cache deletion regressions |
| TNG-042 | Resolved locally: bounded file readers cannot block on FIFOs; strict identity/metainfo reads retain no-follow protection, while configured-file reads verify the opened handle is regular | Keep Unix FIFO/symlink tests and filtered Windows checks |
| TNG-043 | Resolved locally: workflow-run history is capped at 200 rows and 8 MiB, stores 32-item samples with full totals, and truncates error samples to 512 bytes | Keep legacy-history and aggregate-byte regressions; run-history data remains diagnostic, not authoritative |
| TNG-044 | Resolved locally: workflow script-output overflow cancels pipe readers and promptly kills/waits for the child rather than waiting on a blocked stream | Keep the child-overflow timeout regression |
| TNG-045 | Resolved locally: durable qB RSS item-map writes are capped at 8 MiB and legacy reads are bounded before deserialization | Keep aggregate-growth and legacy-size regressions |
| TNG-046 | Resolved locally: sidecar workflow, ratio, and RSS rule JSON collections enforce an 8 MiB aggregate cap and return `413` without replacing prior state | Keep transactional capacity regressions across each store |
| TNG-047 | Resolved locally: native OpenAPI response codes and API prose now match handler behavior; direct-daemon and compatible-sidecar differences are stated explicitly | Re-run the OpenAPI validator and retain handler/source checks when statuses change |
| TNG-048 | Resolved locally: native WebUI torrent-add multipart requests are accepted alongside legacy JSON; both formats bound fields and reject ambiguous sources | Keep WebUI-shaped magnet/file and JSON dual-source regressions |
| TNG-049 | Resolved locally: native JSON rule upserts distinguish invalid input (`400`), count capacity (`429`), and stored-state overflow (`413`) before persistence instead of misreporting them as `503` | Keep status, state-preservation, and entry-cap regressions |
| TNG-050 | Resolved locally: native workflow/ratio-group execution rejects over 10,000 matches before actions and bounds immediate/history samples, totals, errors, and aggregate history bytes | Keep selector-limit, sample-count, UTF-8 error, and history-eviction regressions; this is an API boundary, not a capacity proof |
| TNG-051 | Resolved locally: sidecar multipart torrent uploads now have bounded envelope headroom and route-scoped Axum body limits, while ordinary JSON routes retain their smaller default limit | Keep maximum-file tests for both add APIs and the oversized-JSON rejection regression; not a capacity benchmark |
| TNG-052 | Resolved locally: native automation fields and RSS sample text are bounded, RSS apply/history use bounded samples and totals, oversized legacy rules fail closed, and no-match apply is a no-op | Keep field/tag-vector, request-size, empty-title, RSS result/history, no-match, and legacy-state regressions; API boundary only |
| TNG-053 | Resolved locally: native workflow category filters and `target_category` destinations are distinct, legacy category-only rules migrate, and missing destinations are rejected | Keep category-selection, destination-precedence, migration, and missing-target regressions |
| TNG-054 | Resolved locally: native workflow/RSS history IDs include UUID v4 values, preventing collisions between rapid runs in one second | Keep fixed-timestamp uniqueness regression; timestamps remain separate display metadata |
| TNG-055 | Resolved locally: sidecar storage capacity uses POSIX `f_frsize` for block counts, falling back to `f_bsize` only when the fragment size is zero | Keep the fragment-size/fallback arithmetic regression; no real-device claim is made |
| TNG-056 | Resolved locally: recursive storage-plan walks fail closed beyond 64 directory levels, and Unix directory verification compares sorted names instead of quadratic nested scans | Keep over-depth copy/delete/verify/content-length and unordered/extra-entry regressions; this is not real-device or capacity evidence |
| TNG-057 | Resolved locally: bencode integer and byte-length numeric tokens are bounded during scanning and malformed-token errors no longer copy attacker-controlled token text | Keep oversized integer and length-prefix regressions; no parser throughput or allocation benchmark is implied |
| TNG-058 | Resolved locally: bencode integer parsing rejects an optional leading `+` instead of accepting Rust's non-canonical extension | Keep positive-plus and plus-with-leading-zero regressions; the exact spec interpretation is moderate confidence |
| TNG-059 | Resolved locally: identical BEP 52 files may share a pieces root and one piece-layer dictionary entry; conflicting requirements for the same root still fail | Keep duplicate-root full-metainfo, magnet requirement, and conflict regressions; follows the BEP 52 root-keyed layer model |
| TNG-060 | Resolved locally: v1 metainfo paths are rejected above 64 components, matching storage-plan traversal limits instead of failing only during later storage jobs | Keep exact 64/65 component boundary regressions; this is an explicit input limit |
| TNG-061 | Resolved locally: malformed `private` flags no longer default to public; only absent/0 and 1 are accepted | Keep missing/0/1, wrong-type, and unknown-integer regressions; malformed metadata must fail before public peer discovery is enabled |
| TNG-062 | Resolved locally: private torrents use a single active tracker, fail over only after failure, and clear peers/allowlists when the tracker changes | Keep v1/hybrid and pure-v2 lifecycle regressions; BEP 27 tracker privacy remains fail-closed |
| TNG-063 | Resolved locally: tracker log labels and reqwest transport errors no longer emit passkey-bearing URL components | Keep URL-redaction regressions for HTTP/UDP URLs and malformed input |
| TNG-064 | Resolved locally: engine-admitted parses reserve decoder growth, derived projections, piece-layer temporaries, and raw copies; default convenience APIs use a cumulative 512 MiB per-call cap | Keep denial-before-growth, hybrid, v2 magnet, convenience-cap, and engine lease regressions; custom reservation callbacks remain caller-owned and this is not allocator/RSS qualification |
| TNG-065 | Resolved locally: shared curl policy blocks option-reset/redirect, dynamic-auth, secret-argv/logging, query-credential, URL-list, and resolver-route bypasses | Keep fake-curl bypass, auth/redirect, query/URL, custom-header, and private-file regressions |
| TNG-066 | Resolved locally: rTorrent base64 torrent payloads are length-checked before decode and decoded size is checked against the metainfo cap | Keep encoded-boundary, decoded-overflow, and arithmetic-overflow regressions |
| TNG-067 | Resolved locally: webseed duplicate detection uses borrowed hash-set keys instead of repeated linear scans | Keep the 4,096 repeated-entry regression and preserve first-seen output order |
| TNG-068 | Resolved locally: native JSON torrent uploads enforce the derived base64 envelope ceiling before decoding and retain the decoded raw-size check | Keep exact-boundary, encoded-over-cap, decoded-over-cap, and malformed-small-input regressions |
| TNG-069 | Resolved locally: authenticated curl requests count positional URL operands and reject scheme-less or additional targets | Keep explicit-plus-scheme-less URL and single-label host regressions; require exactly one explicit HTTP(S) URL |
| TNG-070 | Resolved locally: curl `--url=...` preserves case-sensitive path and query bytes when forwarding | Keep mixed-case path/query fake-curl argument regression |
| TNG-071 | Resolved locally: Deluge torrent Base64 decoding selects one alphabet/padding engine instead of retrying four decoders | Keep standard/URL-safe, padded/unpadded, data-URL, and mixed-alphabet regressions |
| TNG-072 | Resolved locally: curl’s negated `--no-globoff` option cannot undo the wrapper’s single-URL routing guarantee | Keep the URL-expansion refusal regression; fake curl must not run |
| TNG-073 | Resolved locally: authenticated GET data cannot introduce credential-like query names or opaque file-backed query fields after URL validation | Keep long/short GET, percent-encoded-name, file-input, and safe-search regressions |
| TNG-074 | Resolved locally: cookie jars cannot target stdout/devices and are restricted to owned regular files at mode `0600` | Keep `-c -` refusal and permissive-mode repair regressions |
| TNG-075 | Resolved locally: curl’s `--libcurl` source export is blocked because generated code can embed request credentials | Keep fake-curl export refusal; generated source was verified against a local file URL |
| TNG-076 | Resolved locally: authenticated curl write-out is restricted to HTTP status and elapsed time, preventing response-header/session-cookie output | Keep `header_json`, `%header{Set-Cookie}`, JSON, and file-backed format refusal regressions while preserving current status/timing callers |
| TNG-077 | Resolved locally: both curl `--json` argument forms spool inline JSON to a mode-`0600` file and participate in GET-query sensitivity checks | Keep process-argv absence, private-file cleanup, and GET sensitive-name/file-input regressions |
| TNG-078 | Resolved locally: curl Referer header and option values are privately spooled and treated as credentials for redirect/diagnostic policy | Keep header/`--referer`/`-e` argv-redaction regressions and fail-closed redirect/`;auto` cases |
| TNG-079 | Resolved locally: curl cookie input filenames must be owned regular non-symlink files and are tightened to mode `0600`; stdin remains explicit via `-` | Keep permissive-mode repair, source-content preservation, and device/descriptor refusal regressions |
| TNG-080 | Resolved locally: explicit/default curl netrc credential sources are owner-checked and tightened to mode `0600` before use | Keep explicit-file, `$HOME/.netrc`, `$NETRC`, optional-netrc, mode-repair, and descriptor refusal regressions |
| TNG-081 | Resolved locally: default `rt-metainfo` convenience APIs cumulatively cap parser allocations at 512 MiB per call and enforce the torrent-byte ceiling | Keep cumulative budget, denial-before-growth, and raw-input boundary regressions; process-wide concurrency and RSS evidence remain separate |
| TNG-082 | Resolved locally: authenticated curl GET rejects both bare and named `--data-urlencode` file inputs, including `--option=value` spelling | Keep bare/named file-backed query regressions and verify named file-backed POST behavior is preserved |
| TNG-083 | Resolved locally: the protected-target plain-HTTP exception now permits only RFC 1918 IPv4 and IPv6 ULA, rather than all addresses Python classifies as not globally reachable | Keep positive private-range, IPv4-mapped, documentation, benchmarking, and special-use regressions |
| TNG-084 | Resolved locally: every proxied location in the bundled Nginx config drops client-supplied `X-Remote-User` before forwarding | Keep the per-location identity-header sanitization regression; custom auth proxies must authenticate first and set their own trusted identity |
| TNG-085 | Resolved locally: compatible-client/native WebUI and API host ports, including the HTTP-only Nginx front door and backend profiles, bind to loopback by default | Keep static and rendered Compose assertions for management ports; BitTorrent peer ports remain intentionally published |
| TNG-086 | Resolved locally: sidecar session cookies use `Secure` by default, logout clears with matching attributes, and public listeners cannot disable it | Keep login/logout cookie-attribute and config-default/non-loopback validation regressions |
| TNG-087 | Resolved locally: sidecar HTTP-client transport, status, and body-read errors no longer retain request URLs that may contain query credentials | Keep refused-connection and HTTP error regressions asserting query secrets are absent from the full error chain |
| TNG-088 | Resolved locally: sidecar tracker diagnostics and mutation failures no longer expose userinfo, path passkeys, query credentials, fragments, or backend-echoed error text | Keep origin-only URL, malformed/magnet masking, and failure-summary secret regressions |
| TNG-089 | Resolved locally: sidecar torrent-ingestion and RSS-magnet failure diagnostics no longer log or return arbitrary backend error text that may echo passkey-bearing inputs | Keep generic RSS failure-summary and redacted source-label regressions |
| TNG-090 | Resolved locally: WebUI workflow webhook and RSS feed list rows no longer display credential-bearing URL userinfo, paths, queries, or fragments | Keep helper and rendered-panel regressions asserting only a validated HTTP(S) origin is shown |
| TNG-091 | Resolved locally: tracker URL labels now show only validated HTTP(S)/UDP origins instead of relying on credential-name and token-length heuristics | Keep short-path, unknown-query, fragment, userinfo, UDP, malformed, and unsupported-protocol regressions |
| TNG-092 | Resolved locally: rTorrent log projection redacts embedded URLs and magnets, plus common credential-like standalone query fields, before persistence or operator API exposure | Keep embedded URL, path-passkey, unknown-query, fragment, non-tracker-scheme, magnet, and standalone signature/auth-key regressions |
| TNG-093 | Resolved locally: untrusted tracker warning/failure strings no longer retain echoed announce credentials or terminal/bidi controls in logs, durable state, or API projections | Keep parser-level and direct-state regressions for userinfo, path/query/fragment secrets, magnets, encoded credential keys, controls, split-line secrets, and safe ordinary text |
| TNG-094 | Resolved locally: workflow script stdout/stderr escape record-breaking, terminal, and bidi controls before entering operational logs | Keep multiline, C0/C1, Unicode line-separator, bidi-control, and ordinary-Unicode regressions |
| TNG-095 | Resolved locally: move and staged-copy import previews no longer claim they can apply on targets whose executor lacks atomic no-replace rename | Keep unsupported-target preview and API issue-label regressions; direct same-filesystem hard-link imports remain applicable |
| TNG-096 | Resolved locally: persisted rTorrent log lines escape terminal, Unicode line-separator, and bidi controls before operator-event storage | Keep persisted-event regressions for ESC/C0/C1 controls, bidi controls, and ordinary text |
| TNG-097 | Resolved locally: qBittorrent, Transmission, rTorrent, and remote TorrentNG status messages redact echoed tracker credentials before API exposure or sidecar persistence | Keep adapter regressions for tracker messages and torrent error messages containing userinfo, path/query secrets, encoded keys, and controls |
| TNG-098 | Resolved locally: legacy SQLite tracker warning/failure rows and sidecar torrent-message cache rows are re-sanitized at API read projection | Keep read-projection regressions with pre-redaction URL credentials, standalone secrets, bidi controls, and safe text |
| TNG-099 | Resolved locally: operator-event messages and JSON payloads are sanitized on write and legacy rows are sanitized on API read | Keep raw-SQL legacy-row and raw-persistence regressions for URLs, paths, credential fields, split-line values, and controls |
| TNG-100 | Resolved locally: dynamic sidecar tracing error fields redact recognized secrets, paths, and controls before emission | Keep the backend error-chain regression and route any new dynamic error fields through the shared redactor |
| TNG-101 | Resolved locally: untrusted non-error tracing fields sanitize category/tag/RSS labels, request paths, torrent identifiers, and configured data paths before emission | Keep structured-field regressions for URL credentials, paths, newline/terminal injection, bidi controls, and useful-context preservation |
| TNG-102 | Resolved locally: native engine webseed diagnostics retain only URL origins, and reqwest failure details are sanitized before logging | Keep origin-only URL and credential/control-bearing webseed error regressions |
| TNG-103 | Resolved locally: qBittorrent URL-fetch diagnostics discard reqwest URLs and reduce source labels to scheme/host/port | Keep local failed-fetch and short path/query/fragment secret regressions |
| TNG-104 | Resolved locally: Tokio task join failures use static task/outcome summaries instead of panic payloads in logs, API errors, and durable failure state | Keep panic-canary regressions across runtime and fastresume boundaries |
| TNG-105 | Resolved locally: the daemon panic hook emits source location but omits the panic payload from stderr | Keep the isolated subprocess regression and install the hook before daemon work starts |
| TNG-106 | Resolved locally: DbWorker shutdown classifies native thread and Tokio wrapper join failures, marks the worker unhealthy, and recovers a poisoned thread-handle lock | Keep nested panic/cancellation/success and unhealthy-state regressions |
| TNG-107 | Resolved locally: supervised shutdown joins now observe task failures and abort-grace expiry across engine, DHT, peer, storage-job, session-event, and uTP paths | Keep bounded timeout/abort/join handling; explicit-abort cancellation is expected, panic remains reportable |
| TNG-108 | Resolved locally: upload-read task failures use the payload-free Tokio join summary instead of formatting JoinError directly | Keep join-error formatting behind `task_join_error_summary` and its panic-canary regression |
| TNG-109 | Resolved locally: sidecar cache writer poisoning fails closed; read checkout skips poisoned connections and errors if none remain | Keep writer, single-reader, and all-readers poison regressions; never resume a potentially interrupted SQLite connection |
| TNG-110 | Resolved locally: sidecar blocking-task joins use payload-free task/outcome summaries instead of retaining or formatting Tokio JoinError | Keep the panic-canary regression and review every `spawn_blocking` boundary when adding workers |
| TNG-111 | Resolved locally: sidecar sync, stats, and optional rTorrent log loops are supervised; an unexpected exit fails the service instead of silently serving stale state | Keep panic, cancellation, and normal-return supervision regressions; deployment supervision remains responsible for restart |
| TNG-112 | Resolved locally: rTorrent low-priority RPC timeouts open the circuit breaker before the failing call returns | Keep the stalled-SCGI regression proving the immediately following background request is rejected |
| TNG-113 | Resolved locally: live-speed cache writes use an exclusive random sibling temp file, so the legacy predictable `.tmp` symlink cannot redirect the write | Keep the planted-symlink regression; the configured parent directory must remain service-owned |
| TNG-114 | Resolved locally: the default rTorrent settings overlay uses persistent writable state instead of the read-only `/config` bind; the UI probes actual directory writability and new overlay files are owner-only on Unix | Keep the entrypoint/Compose wiring, read-only-parent, cleanup, and file-mode regressions |
| TNG-115 | Resolved locally: credential-key classifiers repeatedly decode up to eight percent-encoding layers before redaction or authenticated-URL policy checks | Keep multiply encoded URL/GET keys, split-line values, JSON keys, and ordinary-value regressions |
| TNG-116 | Resolved locally: authenticated curl URL/GET checks and sidecar diagnostics now classify common pass/password/secret aliases consistently | Keep URL, GET-data, log-text, and sensitive-JSON regressions for pass/password aliases, tracker/torrent pass, passphrase, secret-key, bearer/credential, and pid/uk/sig fields |
| TNG-117 | Resolved locally: sidecar text and rTorrent-log redaction now recognize Authorization, API-key, cookie, and other common header-style credential values | Keep multi-token header-value and next-line-preservation regressions |
| TNG-118 | Resolved locally: redacting a sensitive query can no longer bypass path/fragment masking for path-like request targets | Keep path/query/fragment combination regressions |
| TNG-119 | Resolved locally: curl header-file inputs reject FIFOs, symlinks, and other non-regular sources before reading or invoking curl | Keep FIFO timeout and symlink refusal regressions |
| TNG-120 | Resolved locally: curl query-key normalization now strips all non-ASCII-alphanumeric punctuation, matching sidecar redaction | Keep punctuation-obfuscated URL and GET key regressions |
| TNG-121 | Resolved locally: native tracker-warning redaction now matches sidecar coverage for credential aliases, header values, and path-bearing sensitive queries | Keep alias, header, path/query/fragment, and control-character regressions |
| TNG-122 | Resolved locally: sidecar and native tracker redactors now discard trailing fragments whenever a sensitive query value is replaced | Keep sensitive-query + safe-parameter + fragment regressions |
| TNG-123 | Resolved locally: escaped control/format characters and line breaks can no longer split recognized credential keys or header names before redaction; overlong ambiguous chains fail closed | Keep bidi, zero-width, tab, tag-format, multi-line key, split-header, long-chain, and following-line regressions while preserving escaped log output |
| TNG-124 | Resolved locally: remote webhook and user-supplied torrent egress require 2xx responses, so an un-followed 3xx cannot appear successful | Keep loopback webhook redirect, no-follow, and URL-secret-free error regressions |
| TNG-125 | Resolved locally: configured qBittorrent, Deluge, Transmission, and TorrentNG API clients no longer follow redirects; all mutation paths reject 3xx | Keep one loopback redirect regression per adapter and verify no request reaches the redirect target |
| TNG-126 | Resolved locally: browser-marked public login/logout requests and trusted-proxy mutations/WebSockets now enforce same-origin checks; explicit bearer tokens remain non-ambient | Keep cross-origin login/no-cookie, proxy mutation, cookie/proxy/no-auth WebSocket, and bearer-compatibility regressions |
| TNG-127 | Resolved locally: an invalid legacy session cookie no longer shadows a later valid `SID` cookie in loopback-only unsigned-session mode | Keep mixed-cookie fallback regression and constant-time comparison against configured tokens |
| TNG-128 | Resolved locally: unauthenticated token-login routes are throttled per TCP peer with bounded state and a retry hint | Keep 10-attempt/429, successful-login reset, per-peer expiry/isolation, bounded-map, exact-route, and WebUI retry-message regressions |
| TNG-129 | Resolved locally: native peer-ingress admission, budget rejection, read-error, timeout, and malformed-handshake counters are exported to `/metrics` without peer-address labels | Keep counter snapshot, TCP timeout/malformed, EngineHandle, and Prometheus exposition regressions |
| TNG-130 | Resolved locally: the historical next-agent handoff no longer presents already-completed integrations and stale validation caveats as current work | Keep the canonical burn-down as source of truth; re-check dated evidence before reusing historical instructions |
| TNG-131 | Resolved locally: a failing or panicking `ExecuteBatched` command can no longer commit its partial writes alongside successful siblings | Keep write-before-error/panic rollback regressions and abort the entire shared transaction if per-command isolation fails |
| TNG-132 | Resolved locally: native database-worker queue pressure, outcomes, backpressure timeouts, and cumulative queue/command/batch-transaction durations are exported without labels | Keep queue-capacity/timeout, command and deferred-commit counter, EngineHandle snapshot, and Prometheus exposition regressions |
| TNG-133 | Resolved locally: the primary path-backed scheduler now clamps retained file-cache entries to the shared RLIMIT-derived 60% ceiling, with a finite cap for unlimited RLIMIT | Keep low-limit, zero-capacity, unlimited-limit, and scheduler-wiring regressions; TNG-135/136 account active leases and TNG-138 shares their managed-storage quota |
| TNG-134 | Resolved locally: checkpoint sync no longer opens every dirty file at once, avoiding `EMFILE` when the dirty set exceeds the fd cache | Keep the child-process low-`RLIMIT_NOFILE` regression with many dirty paths and a one-entry cache; TNG-135 bounds active leases per scheduler `FilePool` |
| TNG-135 | Resolved locally per path-backed scheduler `FilePool`: descriptor permits follow cached files through caller and backend ownership, applying backpressure across cache eviction and cancellation | Keep lease-drop wakeups, queued-job/CQE lifetime regressions, and the child-process low-`RLIMIT_NOFILE` concurrent-open test; TNG-138 adds a shared managed-storage quota across pools |
| TNG-136 | Resolved locally per `StorageRuntime::HandleCache`: async opens and backend jobs share lifecycle-tied descriptor leases, so cache eviction cannot release admission while I/O still owns the file | Keep async-wait responsiveness, operation/backend lease lifetime, and child-process concurrent-open regressions; its permits now share TNG-138's process-level managed-storage quota |
| TNG-137 | Resolved locally: shared scheduler file-pool and worker-queue snapshots are aggregated once per resource identity, and metrics distinguish cached entries from active descriptors | Keep duplicate-resource and distinct-resource aggregation regressions plus Prometheus assertions for scheduler/runtime cache gauges and top-torrent memory attribution |
| TNG-138 | Resolved locally: scheduler and runtime-cache descriptor permits also draw from one RLIMIT-derived process-level managed-storage budget, with cross-cache wakeups and aggregate metrics | Keep low-RLIMIT multi-cache admission/backpressure tests, async cross-cache wakeup coverage, and Prometheus budget/usage/wait metrics |
| TNG-139 | Resolved locally: IPv6 DHT pending peer-forward admission now enforces the same global peer and pending-torrent caps as IPv4, preventing empty-entry growth when the peer cap is full | Keep the IPv6 full-global-cap regression and parity with the IPv4 pending-forward bounds |
| TNG-140 | Resolved locally: shared native/qBittorrent CSRF admission now requires an explicit `Sec-Fetch-Site: same-origin` claim whenever Fetch Metadata is present, rejecting same-site, none, unknown, and malformed values | Keep the fail-closed Fetch Metadata regression alongside same-origin, cross-site, Origin, Referer, and non-browser compatibility coverage |
| TNG-141 | Resolved locally: native, qBittorrent, Transmission, Deluge, and sidecar Bearer parsing now share exact two-token shape validation and accept case-insensitive schemes | Keep lower-case/mixed-case scheme, extra-token, and invalid-header regressions across the daemon and facade boundaries |
| TNG-142 | Resolved locally: qBittorrent auth and native idempotency exceptions now use exact registered auth paths instead of suffix matching | Keep exact-path regressions so nested or future routes ending in `/auth/login` or `/auth/logout` remain protected |

The practical release statement is therefore: **local functional remediation,
live fault containment, API/SSE load, release-binary smoke, hosted CI, one
official public transfer/soak, and the exercised kspls0 LVM storage gate pass;
branch-protection enforcement, broader public/device coverage, and extended-
scale proof are not complete.**

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
  `QuiesceForStorageMove { reply: oneshot::Sender<Result<bool, String>> }`
  disconnects every peer, drains any peer event already buffered in the
  channel before replying (so a leftover `Block` event from just before
  disconnect can't still reach `handle_block` after the reply fires), and
  replies with whether the torrent was already paused beforehand. The
  acknowledgement fails if persisting the durable `Paused` state fails.
  `ResumeAfterStorageMove
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

Follow-up verified 2026-09-20: orderly shutdown could win after the torrent
was durably quiesced but before the detached save-path planner submitted a
storage job. The engine then discarded `PreparedTorrentFields`; because no
durable job existed, startup recovery had nothing to resume and the previously
active torrent stayed paused. Pending pre-submission moves now retain the
original task handle and pause state. Before the engine discards queued
commands, it sends `ResumeAfterStorageMove` with the unchanged save root for
each such move; already-submitted moves leave this map and continue through
their durable queued-job recovery path. Regression
`shutdown_resumes_torrent_quiesced_for_unsubmitted_storage_move` verifies the
resume command arrives before task shutdown. `cargo test -p rt-engine --lib
--locked` passes all 429 tests, and warnings-denied all-target clippy passes.

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
nothing exercises quiesce/resume specifically across a daemon restart. An
orderly shutdown before plan submission is now covered locally; a hard crash
at that point, and active-peer/cancellation/restart permutations, remain
uncovered.

Deferred evidence action: a live-peer move-under-transfer test (needs a
loopback peer-wire harness); cancellation and hard-crash/restart tests around
an in-flight move. Submitted storage jobs are quiesced and use the existing
worker shutdown fence plus durable restart recovery.

Acceptance: orderly shutdown before submission is covered locally;
move-under-download/upload, cancellation, hard crash, and restart tests remain
deferred evidence. Rollback and no-write-after-commit behavior are implemented
and covered by the focused storage/move tests.

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

### TNG-061 — Malformed private flags default to public

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

`parse_torrent` previously treated every present `private` value except integer
`1`—including wrong types and unknown integers—as public. Downstream engine
policy uses that boolean to gate DHT discovery, peer-source admission, and
PEX. It now treats an absent flag or integer `0` as public, integer `1` as
private, and rejects any other present value or type. Regression tests cover
all four cases. This follows [BEP 27](https://www.bittorrent.org/beps/bep_0027.html),
which requires private torrents to announce only to the private tracker and
connect only to peers returned by it.

### TNG-062 — Private torrents use multiple trackers or retain peers across failover

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Both v1/hybrid and pure-v2 tasks previously scheduled every tracker in the
active tier, sent stopped events across the full tracker list, and could retain
peer connections or the tracker-peer allowlist when switching trackers. They
now select one tracker for private torrents, advance through configured
trackers only after an announce failure, send stopped only to the active
tracker, and disconnect peers/clear the allowlist on failover or tracker-list
replacement. Focused tests cover selection order, stopped selection, and
failover cleanup. This follows [BEP 27](https://www.bittorrent.org/beps/bep_0027.html).

### TNG-063 — Tracker passkeys can leak through logs

**Status: Resolved locally for direct URL and HTTP transport-error paths** · **Priority: P1** · **Confidence: high**

Tracker URLs may carry passkeys in userinfo, path, or query parameters. Tracker
log labels now retain only scheme/authority, and reqwest send/body-read errors
drop the attached URL before being formatted. HTTP and UDP URL regressions
verify that credentials, announce paths, query strings, and fragments are not
returned by the logging-label helper. This closes direct URL and reqwest-error
logging paths; arbitrary tracker-supplied failure text remains untrusted log
content.

### TNG-064 — Metadata parsing is under-reserved by the memory governor

**Status: Resolved locally for engine-admitted and default convenience parsing** · **Priority: P2** · **Confidence: high**

Engine parse leases now grow with bencode collection allocations. Each vector
growth reserves the replacement capacity before allocation while retaining
prior reservations, covering old/new buffer overlap during a moving reallocation.
Before metainfo projections are materialized, a content-derived estimator charges
owned text, v1/v2 file and path graphs, path-collision scratch (including Windows
case-folding storage), dynamically-grown vectors, tracker/webseed sets and
rehashes, piece hashes/layers and Merkle temporaries, and hybrid copies. Every
retained raw torrent/info buffer is separately reserved before cloning. The v2
magnet piece-layer requirement projection has the same pre-admission check.
Native add/restore, metadata completion, and metadata upload parse paths use the
engine lease. The default `parse_torrent`, `torrent_info_bytes`, and
`v2_piece_layer_requirements` convenience APIs now use a cumulative 512 MiB
per-call reservation ceiling and enforce the 64 MiB input ceiling. Explicit
reservation-callback variants remain caller-owned so applications can attach a
shared process governor. This is a structural allocation cap, not allocator
profiling, aggregate-concurrency admission, or an RSS benchmark.

Regressions verify collection reallocation charges, denial before growth,
hybrid projection denial and both raw copies, v2 magnet projection denial,
cumulative convenience-budget enforcement, and engine lease
growth/denial/release. Focused tests pass for `rt-bencode` (18),
`rt-metainfo` (71), and `rt-engine` (441).

### TNG-065 — Shared curl policy allowed credential-routing escapes

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The certification curl wrapper now rejects the `-:`/`--next` transfer reset,
`--location-trusted`, and `--follow`/location redirects whenever request
credentials are present. It accounts for headers, cookies, bodies, netrc, and
AWS-signing auth, and requires authenticated calls to contain exactly one
explicit HTTP(S) URL and exactly one URL argument; scheme-less and additional
positional URL candidates are rejected. URL globbing is disabled. HTTP(S) and other-scheme userinfo, sensitive query
parameter names (including percent-encoded names), URL fragments, `--url-query`,
and URL-list files are rejected before curl starts. Alternate DNS, DoH, Alt-Svc,
proxy, resolve, and connect-to routing are disabled. Curl variable/option
expansion and inline client-certificate, TLS passphrase, and HTTP-signature-key
arguments are rejected; verbose, trace, and response-header output are refused
for authenticated requests. Secret-like custom header values are spooled through
mode-0600 files rather than child argv, sensitive header files are privately
copied, and Proxy-Authorization remains rejected. Workflow-security regressions
use fake curl to verify refusals happen before child execution and retain an
ordinary `?hash=` URL case.

### TNG-066 — rTorrent base64 upload decoded unbounded input before metainfo validation

**Status: Resolved locally** · **Priority: P2** · **Confidence: high**

The rTorrent byte-upload helper now calculates the maximum base64 envelope from
`MAX_TORRENT_BYTES` with checked arithmetic and rejects oversized encoded input
before decoding. It also checks the decoded length, covering inputs just beyond a
non-multiple-of-three limit. `rt-api-rtorrent` tests cover valid small input,
encoded and decoded oversize cases, and arithmetic overflow.

### TNG-067 — Webseed de-duplication used attacker-controlled quadratic scans

**Status: Resolved locally** · **Priority: P2** · **Confidence: high**

Metainfo webseed parsing now keeps first-seen output order while checking
duplicates with a `HashSet<&str>` that borrows decoded input instead of comparing
each new URL against the entire output vector. This removes quadratic repeated
string comparisons under the existing 4,096-URL input limit without adding
duplicate URL copies. A 4,096-entry repeated-URL regression verifies the result.

### TNG-068 — Native JSON torrent upload decoded over-cap Base64 before admission

**Status: Resolved locally** · **Priority: P2** · **Confidence: high**

The native torrent route accepts a 96 MiB request body, permits four concurrent
large-body requests, and caps decoded metainfo at 64 MiB. Its JSON path now
derives the maximum encoded Base64 length from the raw cap and rejects an
oversized `torrent_b64` string before allocating the decoded buffer; the
post-decode raw-size check remains to handle the final partial Base64 quantum.
Malformed input within the envelope still returns `400`, and oversize input
returns `413`. Small synthetic-boundary tests cover exact-limit success,
pre-decode envelope rejection, decoded-over-cap rejection, and malformed input.

### TNG-069 — Authenticated curl calls could append a scheme-less second URL

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The curl wrapper counted only URL arguments beginning with `http://` or
`https://`. Curl also accepts scheme-less URL operands and guesses the protocol,
so one explicit target plus a bare second hostname could pass the one-URL check
while the wrapper attached its private Authorization header to the invocation.
The policy now counts every unconsumed positional URL operand as well as
explicit `--url` operands, rejects any scheme-less authenticated target, and
parses common value-taking options (`-o`, `-w`, `-X`, cookie jar, and timeout /
retry controls) so their values are not mistaken for URLs. Fake-curl regressions
prove that both a dotted host and a single-label host appended to an explicit
URL are refused before curl runs.

### TNG-070 — `--url=...` lowercased case-sensitive request paths

**Status: Resolved locally** · **Priority: P3** · **Confidence: high**

The `--url=VALUE` form normalized the entire value to lowercase for scheme
counting and then forwarded that normalized value. The wrapper now keeps the
original URL for forwarding and uses a lowercase copy only for scheme checks.
A fake-curl regression verifies the mixed-case path and query arrive unchanged.

### TNG-071 — Deluge retried multiple Base64 engines for one torrent payload

**Status: Resolved locally** · **Priority: P2** · **Confidence: high**

The Deluge add path retried standard, URL-safe, padded, and unpadded Base64
engines sequentially. Malformed near-limit input could therefore be scanned and
partially decoded several times before rejection. The decoder now selects one
engine from the alphabet-specific characters and presence of padding, retaining
all four accepted encodings while rejecting mixed alphabets in one pass.
Regressions cover standard and URL-safe encodings with and without padding,
data-URL input, and a mixed-alphabet rejection.

### TNG-072 — Curl URL globbing could be re-enabled after policy setup

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The wrapper supplied `--globoff` before caller options, but curl supports the
negated `--no-globoff` option. A caller could therefore expand one authenticated
URL expression into multiple transfers after the wrapper counted it as one URL.
The wrapper now rejects `--no-globoff` before invoking curl. A fake-curl test
confirms an authenticated brace-list URL is refused and the child is not run.

### TNG-073 — GET data bypassed sensitive URL-query checks

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The curl wrapper validated the URL before launch but did not account for
`-G`/`--get`, which appends `--data*` values to that URL. Credential-like field
names (including percent-encoded names) and file-backed GET data are now refused
before curl starts; safe search fields remain supported. Regressions cover both
GET spellings, an encoded `api_key`, opaque file-backed input, and an allowed
`query=` field.

### TNG-074 — Cookie jars could disclose cookies through stdout or shared files

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl writes cookies to stdout when the `--cookie-jar` destination is `-`.
Cookie-jar paths are now required to be existing, owned, regular non-symlink
files; stdout/device targets are refused and accepted jars are changed to mode
`0600` before curl runs. Tests verify rejection of `-` and tightening an
existing mode-`0644` jar.

### TNG-075 — `--libcurl` could export request credentials into generated source

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl's `--libcurl` option writes equivalent libcurl C source to a file. A local
offline probe confirmed that the generated source includes an explicit
Authorization header value. The shared wrapper now rejects `--libcurl` and its
equals form before child execution; a fake-curl regression covers the dash
destination form that previously escaped URL-operand counting.

### TNG-076 — Authenticated curl write-out could print response credentials

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl's [write-out format](https://curl.se/docs/manpage.html#write-out) can emit
individual response headers or the complete response-header map, including
`Set-Cookie`; its format can also be loaded from a file. The wrapper previously
disabled `--include` and header dumps for authenticated calls but allowed
arbitrary write-out formats. Authenticated write-out is now limited to the HTTP
status and elapsed-time fields used by the repository, with only
newline/tab/percent formatting escapes; file-backed formats and other variables
are rejected before curl runs. Regressions cover `%{header_json}`,
`%header{Set-Cookie}`, `%{json}`, and `@file`, while the existing authenticated
`%{http_code}` test verifies the supported path.

### TNG-077 — Curl `--json=...` bypassed request-body spooling

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl's [`--json` shortcut](https://curl.se/docs/manpage.html#json) sends its argument as request data, but the wrapper
only recognized the separate `--json VALUE` form as an unknown option and did
not treat `--json=VALUE` as a body option. The equals form could therefore
leave inline JSON credentials in curl's process arguments. Both spellings now
use private mode-`0600` temporary files that are removed after curl returns,
and the body is included in the existing authenticated-URL and GET-query
policy. Fake-curl tests verify neither form exposes its JSON values in child
arguments, cleanup occurs, safe authenticated write-out remains supported, and
credential-like or opaque file-backed JSON GET data is refused.

### TNG-078 — Referer URLs could remain in curl's process arguments

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl's [`--referer` option](https://curl.se/docs/manpage.html#referer) sets a
request header from a URL, but the wrapper did not recognize
`--referer=VALUE`; that spelling could carry a session-bearing path or query
directly in curl's argv. `Referer` headers and all option spellings (`--referer`,
`--referer=`, `-e VALUE`, and `-eVALUE`) now use the private mode-`0600` header
file and trigger the authenticated single-target, no-redirect, and no-diagnostic
rules. The curl `;auto` redirect form is refused rather than rewritten into a
literal header. Fake-curl regressions assert the URL values do not appear in
child argv and that redirect/automatic mode fails before curl runs.

### TNG-079 — Cookie input files could remain broadly readable

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl treats a `--cookie` value without `=` as a cookie-file path and reads
authentication cookies from it ([curl cookie option](https://curl.se/docs/manpage.html#cookie)).
The wrapper previously marked the call authenticated but did not validate that
input file or its permissions. Cookie input files now must be existing owned
regular non-symlink files and are tightened to mode `0600` before curl runs;
device/descriptor paths are refused, while the explicit `-` stdin form remains
available. A regression verifies mode tightening without changing file
contents, plus refusal of `/dev/stdin`.

### TNG-080 — Netrc credentials could be group- or world-readable

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

Curl reads login/password pairs from explicit `--netrc-file` paths or the
default `.netrc` location. Its [netrc option documentation](https://curl.se/docs/manpage.html#netrc)
warns that such files should not be group- or world-readable but says curl does
not enforce that. The wrapper now requires explicit netrc sources to be
existing, owned, regular non-symlink files and tightens them to mode `0600`.
For default netrc modes it protects any active `NETRC` override and the
`$HOME/.netrc` fallback (covering curl versions with and without `NETRC`
environment support); optional mode may still run without a file. Fake-curl
tests use only isolated temporary files and verify mode repair and content
preservation.

### TNG-081 — Standalone metainfo helpers had no aggregate allocation ceiling

**Status: Resolved locally** · **Priority: P2** · **Confidence: moderate**

The engine supplied an allocation-reservation callback, but the default
`parse_torrent`, `torrent_info_bytes`, and `v2_piece_layer_requirements`
helpers accepted all requested allocations; the two extraction helpers also
lacked the 64 MiB raw-info ceiling. Default convenience calls now share a
cumulative 512 MiB per-call reservation cap, and all three enforce the input
size limit before decoding. Applications with a process-wide memory governor
can continue to use the explicit callback variants. A regression verifies
cumulative accounting and denial before the first bencode collection growth.
The cap is not an RSS bound and does not serialize concurrent callers.

### TNG-082 — Named curl URL-encoded files bypassed GET query inspection

**Status: Resolved locally with fake-curl regressions** · **Priority: P1** · **Confidence: high**

Curl's `--data-urlencode` supports both `@filename` and `name@filename`; each
form reads the file contents and URL-encodes them. With `--get`, curl appends
that data to the URL query ([`--data-urlencode`](https://curl.se/docs/manpage.html#data-urlencode),
[`--get`](https://curl.se/docs/manpage.html#get)). The wrapper detected bare
`@filename` but parsed `name@filename` as an inline field name, allowing
opaque file contents into a GET under a non-sensitive-looking name. Both
file-backed forms are now marked uninspectable and rejected whenever GET mode
is active, including long-option `=value` and combined-short-option forms.
File-backed named POST data remains forwarded unchanged. Fake-curl regressions
prove refusal occurs before curl launches and preserve the POST form.

### TNG-083 — Protected-target HTTP exception admitted special-use IP ranges

**Status: Resolved locally with address-classification regressions** · **Priority: P1** · **Confidence: high**

The private-HTTP exception used `ipaddress.is_private` as if it meant RFC 1918
IPv4 or IPv6 unique-local space. Python defines that property in terms of
IANA's “not globally reachable” registries, with additional versioned
special-range classifications, so ranges such as `198.51.100.0/24`,
`203.0.113.0/24`, `198.18.0.0/15`, and `2001:db8::/32` could pass an explicitly
matching cleartext exception. The exception now accepts only `10/8`,
`172.16/12`, `192.168/16`, and `fc00::/7`; IPv4-mapped IPv6 addresses are
classified by their embedded IPv4 address. Regressions accept those private
ranges and reject documentation, benchmarking, other special-use, and mapped
non-private examples. See Python's [`ipaddress.is_private` definition](https://docs.python.org/3/library/ipaddress.html#ipaddress.IPv4Address.is_private).

### TNG-084 — Bundled reverse proxy forwarded an untrusted identity header

**Status: Resolved locally with a CI regression** · **Priority: P2** · **Confidence: high**

The sidecar's opt-in `auth.trust_proxy_header` path treats any non-empty
`X-Remote-User` as authenticated, and configuration restricts that mode to a
loopback listener. The bundled Nginx front door nevertheless forwarded
original request headers by default on its WebUI, API, and WebSocket locations.
The Nginx configuration now clears `X-Remote-User` separately in all three
locations. A workflow-security regression checks every proxied location, so a
future route cannot silently restore client-controlled identity forwarding.
This was a conditional configuration hazard rather than an unconditional
bypass: the shipped sidecar config disables proxy-header trust. Custom proxies
that enable it must authenticate the caller before setting their own identity
header. Nginx documents that request headers pass upstream by default and that
an empty `proxy_set_header` value suppresses that field
([proxy module](https://nginx.org/en/docs/http/ngx_http_proxy_module.html)).

### TNG-085 — HTTP management ports were published on all interfaces

**Status: Resolved locally with Compose regressions** · **Priority: P1** · **Confidence: high**

The compatible-client Compose stack published its HTTP API on `8080` and its
HTTP-only Nginx front door on `80` to every host interface. The optional
qBittorrent, Transmission, and Deluge adapter profiles likewise published
their HTTP APIs on `8082`–`8084`; the native client Compose stack published its
HTTP API on `28082`. Authentication does not protect credentials or session
cookies from network interception over cleartext HTTP. These management/API
host ports now bind to `127.0.0.1` by default, while BitTorrent peer ports
remain published for inbound connections. Static CI regression coverage and
rendered Docker Compose checks enforce the bindings. Remote UI/API exposure
requires an authenticated TLS reverse proxy or an explicit protected override.

### TNG-086 — Session cookies omitted the `Secure` attribute

**Status: Resolved locally with auth/config regressions** · **Priority: P1** · **Confidence: high**

The sidecar issued signed, `HttpOnly`, `SameSite=Lax` session cookies without
`Secure`, so a browser could send them over cleartext HTTP even when the user
normally accessed the service over HTTPS. RFC 6265 defines `Secure` as limiting
cookie transmission to secure channels and recommends it for cookies used
over secure channels ([RFC 6265, section 4.1.2.5](https://www.rfc-editor.org/rfc/rfc6265#section-4.1.2.5)).
The new `auth.secure_cookies` setting defaults to `true`, applies to both login
and logout headers, and cannot be disabled on a non-loopback listener. A
deliberate opt-out remains for trusted loopback HTTP development and is covered
separately from the default-secure path.

### TNG-087 — Sidecar HTTP-client errors exposed request query secrets

**Status: Resolved locally with transport and status regressions** · **Priority: P1** · **Confidence: high**

`reqwest` transport and HTTP-status errors can retain and format their request
URL. Sidecar backend URLs may carry credentials in query parameters, including
workflow webhook URLs. Production HTTP send, status, and bounded-body-read
errors now remove the retained URL before adding contextual error messages.
Focused local tests reproduce both a refused connection and an HTTP 500 and
assert that neither the credential value nor the query string appears in the
full error chain. The regression servers bind only to loopback; no external
request is made.

### TNG-088 — Sidecar tracker diagnostics exposed passkeys

**Status: Resolved locally with redaction and failure-summary regressions** · **Priority: P1** · **Confidence: high**

The sidecar's tracker URL helpers removed query strings but preserved URL
userinfo and paths, where tracker credentials may also appear. Deluge and
Transmission “tracker not found” errors interpolated the full URL, and the
native tracker-mutation and cross-seed APIs reflected submitted URLs and
backend error text into failure responses. qBittorrent-compatible mutation
logs also included backend error text that could echo the submitted URL. A
shared redactor now retains only scheme, host, and port for hierarchical URLs
and masks magnets, local paths, and malformed values. Backend not-found errors
no longer include URLs; tracker mutation responses and logs use generic
failure text without backend error bodies. Tests cover credentials in
userinfo, path, query, and fragment, plus the shared API failure-summary
builder used by tracker mutation and cross-seed. Confidence: high.

### TNG-089 — Torrent-ingestion diagnostics could expose echoed secrets

**Status: Resolved locally with generic failure summaries** · **Priority: P1** · **Confidence: moderate**

Sidecar magnet, URL, and metainfo ingestion paths logged arbitrary backend
error text. RSS magnet application also returned backend error text to the
caller. Deluge and Transmission can return server-supplied error strings, and
rTorrent errors cross a separate RPC boundary; such text may echo a submitted
magnet URI or metainfo tracker passkey. The sidecar now records only a
redacted magnet/URL source label and generic operation failure in these
paths; RSS application returns a rule-specific generic error without backend
text. The possible echo depends on backend behavior, so confidence is
moderate. No external backend or network was contacted during verification.

### TNG-090 — WebUI configuration rows exposed complete endpoint URLs

**Status: Resolved locally with helper and rendered-panel regressions** · **Priority: P1** · **Confidence: high**

Workflow rows rendered configured webhook URLs verbatim, and RSS rule rows
rendered complete feed URLs. Either can carry credentials in userinfo, opaque
path tokens, query parameters, or fragments, making them visible during normal
screen sharing or capture. Both rows now show only a parsed HTTP(S) origin and
a generic path marker; malformed and non-HTTP(S) values fail closed to a
generic label. The builder inputs remain editable by the configuring user.
Unit and component regressions verify that URL credentials and endpoint
details do not appear in either rendered panel. WebUI tests, typecheck, lint,
and an isolated production build pass.

### TNG-091 — Tracker URL masking heuristics left secret components visible

**Status: Resolved locally with redaction regressions** · **Priority: P1** · **Confidence: high**

The tracker UI masker recognized selected credential query keys and long
alphanumeric path tokens, but left unknown query names, short path credentials,
and fragments visible. Tracker summary labels now retain only the validated
HTTP(S)/UDP origin and use a generic redacted label for malformed or
unsupported values. The tracker detail control keeps its explicit Reveal and
Copy actions for users who need the full URL. Regressions cover userinfo,
short path secrets, an unrecognized `signature` query, fragments, UDP, and
malformed/opaque inputs. WebUI tests, typecheck, lint, and the isolated
production build pass.

### TNG-092 — rTorrent log projection retained URL credentials

**Status: Resolved locally with persisted-log redaction regression** · **Priority: P1** · **Confidence: high**

The rTorrent log sanitizer masked selected query names and standalone magnet
lines, but full tracker URLs in log output could preserve userinfo, path
passkeys, unrecognized query values, and fragments. These messages are stored
as operator events and returned by the logs API. Scheme-qualified URLs now use
the sidecar's shared origin-only redactor, embedded magnets are masked, and
common standalone credential-like query keys include signature/auth variants.
The regression exercises HTTP(S), UDP, FTP, userinfo, a short path token, an
unrecognized `signature` URL query, fragments, standalone signature/auth-key
values, and an embedded magnet. The complete sidecar test suite and
warnings-denied Clippy pass.

### TNG-093 — Tracker-supplied messages could echo announce credentials

**Status: Resolved locally with parser and state-boundary regressions** · **Priority: P1** · **Confidence: high**

Tracker failure reasons and warning messages are untrusted response text. They
were length-bounded but retained verbatim in `TrackerState`, persisted with
tracker projections, logged on announce failure, and exposed through native
API tracker status. A tracker receiving a passkey-bearing announce URL can
echo that URL or its credentials in either field. The response parser now
redacts scheme-qualified URLs, magnets, and common credential-like query
fields (including percent-encoded key names) and removes terminal control
characters and bidi formatting controls before returning text; the state
retention boundary repeats the sanitation for direct callers. Split-line
credential fields fail closed even when a line break separates a sensitive key
from its value. Regression tests cover parser failure/warning output, direct
state updates, URL userinfo, short path keys, unknown query keys, fragments,
standalone auth/signature fields, magnets, terminal/bidi controls, and split
credential fields. The full workspace test and warnings-denied Clippy gates
pass.

### TNG-095 — Storage plans overstated support without atomic no-replace rename

**Status: Resolved locally with plan and API regressions** · **Priority: P2** · **Confidence: high**

The Unix executor deliberately returns `Unsupported` for publication renames
on targets other than Linux/Android/macOS, but `plan_move` and staged-copy
`plan_import` still returned `can_apply = true`. A queued operation would fail
only when it reached its rename step. The planner now adds an explicit
`AtomicNoReplaceRenameUnavailable` issue and sets `can_apply = false` for
moves and staged-copy imports on those targets; direct same-filesystem
hard-link imports remain usable because they do not need that rename. Native
API issue projection names the limitation. Regression tests simulate the
unsupported target and cover the supported direct-import path. The storage
suite (199 tests) and native API suite (99 tests) pass; storage/API test targets
compile for FreeBSD and Windows GNU, and storage compiles for macOS. The full
macOS API cross-check is unavailable because the installed host C compiler
cannot build its `ring`/SQLite dependencies for Apple target flags.

### TNG-094 — Workflow script output could forge operational log records

**Status: Resolved locally with output-sanitization regression** · **Priority: P2** · **Confidence: high**

Workflow script output was bounded to 64 KiB per stream but logged verbatim.
Newlines and terminal controls could inject apparent records or terminal
sequences; Unicode line separators and bidi controls could also distort how
operators read a record. Before logging, output now escapes C0/C1 controls,
Unicode line/paragraph separators, and bidi formatting controls while
preserving ordinary Unicode. The regression covers forged multiline output,
ESC, C1, line separators, bidi overrides/isolates, and readable non-ASCII
text. All sidecar unit/integration tests and warnings-denied Clippy pass.

### TNG-096 — Persisted rTorrent logs retained terminal and bidi controls

**Status: Resolved locally with persisted-event regression** · **Priority: P2** · **Confidence: high**

The rTorrent log importer already redacted URLs, magnets, and common standalone
credential-like fields, but persisted raw C0/C1 and Unicode bidi/line controls.
Those controls could make a single event render as forged lines or misleading
terminal text. The shared operator-log sanitizer now escapes those controls
after redaction and before the event is stored; readable Unicode remains
unchanged. A persisted-event regression checks ESC, C0/C1, bidi controls, and
ordinary text. Sidecar tests pass (218 unit / 112 integration), warnings-denied
Clippy and formatting pass, and the test target cross-compiles for Windows GNU.

### TNG-097 — Compatible-backend status messages could echo tracker credentials

**Status: Resolved locally with adapter projection regressions** · **Priority: P1** · **Confidence: high**

The qBittorrent tracker `msg`, Transmission `lastAnnounceResult` and
`errorString`, rTorrent `d.message`, and a configured remote TorrentNG backend's
`tracker_message` can contain text returned from trackers, but their sidecar
projections previously passed it through unchanged.
Those values reach the tracker/torrent APIs and some are persisted in the
sidecar cache. They now use a shared text redactor before projection: embedded
URLs retain only their origin, magnets are hidden, common credential-like
query keys (including percent-encoded names) are masked, and control characters
are escaped. Regression tests exercise all three backend paths and verify
ordinary status text remains readable. Sidecar tests pass (223 unit / 112
integration), warnings-denied Clippy and formatting pass, and the test target
cross-compiles for Windows GNU.

### TNG-098 — Previously persisted tracker messages bypassed new redaction

**Status: Resolved locally with legacy-row projection regression** · **Priority: P1** · **Confidence: high**

The TNG-093 parser and state boundaries protect new tracker responses, but
tracker warning/failure columns written by older versions remain in SQLite.
The compatibility tracker APIs read those rows through
`engine_tracker_snapshot`, which previously copied the stored strings directly
into qBittorrent, Transmission, Deluge, and rTorrent response models. The
engine now re-applies the tracker sanitizer at that read projection. Separately,
legacy sidecar `torrents.message` cache values written before TNG-097 are
re-sanitized by the common `torrent_row_from_sql` mapper, protecting get/list/
delta API reads even while the configured backend is unavailable. Neither read
path mutates the database. The sanitizer also masks a sensitive key whose
value is split onto the next line and removes bidi formatting controls.
Regressions seed old database rows with URL credentials, standalone auth
material, and bidi controls. `rt-tracker` tests pass (89), sidecar tests pass
(224 unit / 112 integration), the engine projection regression passes, and
full locked workspace tests, warnings-denied Clippy, formatting, and
`git diff --check` pass.

### TNG-099 — Persisted operator events bypassed newer redaction

**Status: Resolved locally with write/read persistence regressions** · **Priority: P1** · **Confidence: high**

TNG-092/TNG-096 protect newly ingested rTorrent lines, but `app_events` also
contains other operator messages and structured payloads, and older rows were
returned unchanged. The native `/api/v1/logs` endpoint exposes both fields;
the qBittorrent log endpoint exposes the message through the same database
projection. `append_app_event` now sanitizes message text and recursively
sanitizes JSON string values, replacing values under recognized credential
keys. `list_app_events_filtered` applies the same projection to existing rows,
so no database rewrite is needed. Recognized URLs, magnets, standalone paths,
credential-like query values (including values continued after a line break),
and terminal/bidi controls are handled; JSON structure and ordinary text are
preserved. Invalid legacy payload JSON fails closed to `{}`.

Regressions query the raw SQLite row after a new write and seed an old row
directly, then verify that the read projection hides credentials and parent
paths while keeping safe context; POSIX, drive-letter, and UNC paths are
covered. Sidecar tests pass (228 unit / 112
integration), warnings-denied Clippy, formatting, `git diff --check`, and the
Windows GNU sidecar test-target check pass. No public-network test,
torrent-count proof, or soak was run. Basenames and arbitrary unclassified
non-URL secrets are not exhaustively hidden.

### TNG-100 — Direct tracing logs bypassed operator-event redaction

**Status: Resolved locally with backend error-chain regression** · **Priority: P1** · **Confidence: high**

Many sidecar tracing calls wrote dynamic errors using `%e` or `%error` directly,
including failures from backends, API adapters, and filesystem workers. Sync
and transfer-stat failures also wrote their complete backend `anyhow` error
chains, separately from the sanitized `app_events` writer. rTorrent log-poll
failures included configured file paths. A shared display redactor now
sanitizes dynamic error fields across the sidecar API/qBittorrent handlers,
startup, stats, and sync tracing calls; `sync::error_chain` uses the same
redactor. The rTorrent poll, recovery, and event-write paths also sanitize
dynamic error and source fields before emission. A source scan found no raw
`error = %e` or `error = %error` fields left in sidecar Rust sources.

The regression passes an error chain containing URL userinfo, query secrets,
a local path, a split-line credential, and terminal/bidi controls, and verifies
that useful origin/context remains without the sensitive values or raw
controls. Sidecar tests pass (229 unit / 112 integration), warnings-denied
Clippy, formatting, `git diff --check`, and the Windows GNU sidecar test-target
check pass. Non-error tracing fields are tracked separately under TNG-101. No
public-network test, torrent-count proof, or soak was run.

### TNG-101 — Non-error structured tracing fields carried untrusted values

**Status: Resolved locally with structured-field redaction regression** · **Priority: P1** · **Confidence: high**

An audit of non-error tracing values found user-controlled category/tag names,
RSS rule labels, request method/path values, raw torrent identifiers returned
by backends, and a configured data-directory path. Length bounds do not prevent
embedded newlines, terminal controls, bidi overrides, credentials, or local
paths from being written into operator logs. These fields now pass through the
shared display redactor; rTorrent source labels were already sanitized when
constructed, and numeric rule IDs remain typed numeric values.

The structured-field regression uses a category-like value containing a
newline, ESC, bidi override, credential-bearing URL, query token, and local
path. It verifies that origin and safe path context remain while credentials,
parent paths, and raw controls do not. Sidecar tests pass (230 unit / 112
integration), warnings-denied Clippy, formatting, `git diff --check`, and the
Windows GNU sidecar test-target check pass. A source scan found no remaining
raw dynamic string values among the audited structured fields. Arbitrary
unclassified non-URL secrets are not exhaustively hidden. No public-network
test, torrent-count proof, or soak was run.

### TNG-102 — Native webseed diagnostics exposed credential-bearing URLs

**Status: Resolved locally with redaction regressions** · **Priority: P1** · **Confidence: high**

Native webseed debug and warning events previously included the configured URL
and expanded block URL, which can contain userinfo, path passkeys, query
credentials, or fragments. Transport errors can also carry the request URL.
Webseed and tracker URL labels now share an origin-only formatter, reqwest
transport errors discard their URL, and the warning error text passes through
the tracker-message sanitizer before emission.

The URL-label regression covers credential-bearing HTTP and UDP forms, path,
query, fragment, malformed values, and a webseed URL. The webseed error-text
regression covers URL credentials and terminal controls. `rt-engine` tests
pass (443); the locked offline workspace tests, warnings-denied workspace
Clippy, formatting, and `git diff --check` pass. No public-network request,
torrent-count proof, or soak was run.

### TNG-103 — qBittorrent URL-fetch errors retained request URLs

**Status: Resolved locally with a loopback failure-path regression** · **Priority: P1** · **Confidence: high**

The qBittorrent torrent-URL fetch path logged reqwest send/body-read errors
directly, allowing transport diagnostics to retain the submitted URL. Its
source label also kept the path and arbitrary query/fragment values while
masking only selected query-key names. Send and body-read errors now remove the
reqwest URL; source labels retain only the scheme, host, and port. Egress-policy
errors expose only their validated host/address context, not URL userinfo,
path, query, or fragment.

The local regression drops a loopback TCP connection after accept and asserts
that username, password, path, query token, and fragment are absent from the
returned error. A formatter regression asserts that short path passkeys and
unknown query names/values are all removed. `rt-api-qbit` tests pass (91); the
locked offline workspace tests, warnings-denied workspace Clippy, formatting,
and `git diff --check` pass. The test uses only a local listener; no
public-network request, torrent-count proof, or soak was run.

### TNG-104 — Tokio task panic payloads escaped through error boundaries

**Status: Resolved locally with panic-payload regressions** · **Priority: P2** · **Confidence: high**

Tokio's `JoinError` display includes a string panic payload. Several supervised
worker and parallel-projection paths formatted it directly, allowing a panic
message to reach operator logs, compatibility API errors, torrent error
state, or durable storage-job recovery text. These are failure-only paths,
but panic messages can include input-derived values.

The engine now summarizes join failures using a static task label and whether
the task panicked or was cancelled. The same boundary is used by native engine
workers, storage-job recovery/logging, peer-handshake supervision, compatibility
API projections, daemon CLI workers, and fastresume saves. Ordinary returned
operation errors are unchanged. Regressions use a panic canary and verify that
it is absent from both the engine summary and fastresume error.

The locked offline workspace suite passes, including 444 `rt-engine` tests;
`rt-fastresume` passes 29 tests. Warnings-denied workspace Clippy, formatting,
and `git diff --check` pass. This closes explicit `JoinError` formatting
boundaries; the separate process panic-hook stderr surface is addressed under
TNG-105. No public-network request, torrent-count proof, or soak was run.

### TNG-105 — The default panic hook printed panic payloads to stderr

**Status: Resolved locally with isolated subprocess regression** · **Priority: P2** · **Confidence: high**

The daemon's default Rust panic hook wrote the panic payload to stderr before
Tokio could return a `JoinError`. This bypassed the static join summaries from
TNG-104 and could place input-derived text in process logs. The daemon now
installs a hook as its first action; it writes only the source location and a
fixed message that the payload was omitted. It uses a fallible stderr write so
a logging failure does not cause another panic from the hook.

The subprocess regression runs an isolated ignored child test that panics with
a canary, then asserts that stderr contains `main.rs:<line>` and the fixed
omission message but not the canary. The focused test passes; full workspace
tests, warnings-denied Clippy, formatting, and `git diff --check` are rerun for
handoff. This policy applies to the `torrentngd` process; applications that
embed the engine library own their process panic hook. No public-network
request, torrent-count proof, or soak was run.

### TNG-106 — DbWorker shutdown ignored nested join failures

**Status: Resolved locally with nested-join regressions** · **Priority: P2** · **Confidence: high**

`DbWorker::join_thread` previously treated only the outer timeout as a failure.
The awaited `spawn_blocking` result is nested: the database OS thread can
return a panic from `std::thread::JoinHandle::join`, and the Tokio wrapper can
itself panic or be cancelled. Those completed failures previously looked like
a successful join. Shutdown now classifies each outcome with fixed text,
marks the worker unhealthy, and sets its stop flag. It also recovers the
thread-handle mutex after poisoning instead of silently skipping the join.

Regressions cover the native worker panic, Tokio wrapper panic/cancellation,
successful join classification, poisoned handle-lock recovery, and the
unhealthy/force-stop state after a panicked worker. The locked offline
workspace suite passes, including 451 `rt-engine` tests; `rt-utp` passes 43
tests. Warnings-denied workspace Clippy, formatting, and `git diff --check`
pass. No public-network request, torrent-count proof, or soak was run.

### TNG-107 — Supervised shutdown paths discarded completed task failures

**Status: Resolved locally with bounded-join and uTP regressions** · **Priority: P2** · **Confidence: high**

Several shutdown paths checked only whether `timeout(...)` elapsed. A completed
Tokio join failure is nested inside the successful timeout result, so actor,
peer-listener, DHT (including IPv6), storage-job supervisor, and session-event
writer panics could be silently treated as clean shutdown. The shared join
path now reports payload-free panic/cancellation summaries, aborts on deadline,
reports abort-grace expiry, and treats cancellation caused by that abort as
expected. Per-torrent demotion now inspects both graceful and post-abort join
results; v2 peer cleanup reports panics after abort. uTP endpoint shutdown
returns static panic/cancellation errors, which the peer listener records.
Poisoned uTP task-handle locks are recovered so shutdown/drop still own the
receive task.

Regressions exercise the shared helper's panic, success, timeout, and abort
paths, plus uTP receive-task panic reporting, socket release, and poisoned-lock
recovery. The locked offline workspace suite passes (451 `rt-engine`, 43
`rt-utp` tests); warnings-denied workspace Clippy, formatting, and
`git diff --check` pass. No public-network request, torrent-count proof, or
soak was run.

### TNG-108 — Upload-read diagnostics formatted Tokio JoinError directly

**Status: Resolved locally with payload-free join summaries** · **Priority: P2** · **Confidence: high**

The peer upload-read failure branch logged Tokio's `JoinError` using its
`Display` implementation. That can include a string panic payload, bypassing
the static join summaries added for other supervised paths. It now routes the
failure through `task_join_error_summary`; the existing panic-canary summary
regression verifies that the formatter omits payload text. The locked offline
workspace suite, warnings-denied Clippy, formatting, and `git diff --check`
pass. No public-network request, torrent-count proof, or soak was run.

### TNG-109 — Sidecar cache mutex poisoning caused repeat panics

**Status: Resolved locally with poison-recovery regressions** · **Priority: P2** · **Confidence: high**

The cache writer accessor previously panicked on every use after its mutex was
poisoned, and read-pool checkout could panic on its selected poisoned slot even
when healthy readers remained. Writer acquisition now reports a static error
and leaves the connection unavailable; readers skip poisoned slots and report
a static error only when the pool has no healthy connections. Each pool logs
its poison condition once. The SQLite connection is deliberately not recovered
from the poisoned guard, because a panic may have interrupted transaction
state.

Regressions poison a writer, one reader, and then the full reader pool from
panicking threads. They verify the writer stays poisoned and fails without a
repeat panic, healthy readers still answer queries, and a fully poisoned pool
returns an error. The sidecar locked offline suite passes (235 library / 3
binary / 112 compatibility tests); warnings-denied Clippy, formatting, and
`git diff --check` pass. No public-network request, torrent-count proof, or
soak was run.

### TNG-110 — Sidecar blocking-task joins could expose panic payloads

**Status: Resolved locally with payload-canary regression** · **Priority: P2** · **Confidence: high**

Several sidecar `spawn_blocking` boundaries redacted `JoinError` display text
as if it were an ordinary URL-bearing error; redaction does not remove an
arbitrary panic payload. The cache blocking wrapper also retained `JoinError`
in its `anyhow` source chain. All sidecar blocking-task join failures now use
the shared task/outcome summary, and the cache wrapper discards the join error
as a source. The session tracker lookup additionally warns with a safe summary
instead of silently defaulting on worker failure; the workflow path-validation
response contains only the safe summary.

A canary panic through `Db::run_blocking` verifies the returned error chain
contains the task/outcome but not the panic text. The sidecar locked offline
suite passes (235 library / 3 binary / 112 compatibility tests);
warnings-denied Clippy, formatting, and `git diff --check` pass. No
public-network request, torrent-count proof, or soak was run.

### TNG-111 — Detached sidecar service loops could fail silently

**Status: Resolved locally with task-supervision regressions** · **Priority: P2** · **Confidence: high**

`main.rs` previously discarded the handles for sync, stats, and optional
rTorrent log-ingestion loops. If one panicked or was cancelled, Tokio retained
no observer to report the failure and the HTTP server could continue serving
stale cache or stats. The service now retains those handles, monitors them
alongside HTTP serving, emits a payload-free task summary, and returns an error
on panic, cancellation, or unexpected normal completion. The remaining loops
are aborted when the service exits. Background workers start only after the
HTTP listener binds successfully.

Regressions cover panic-payload suppression, cancellation, and unexpected
normal return. The sidecar locked offline suite passes (235 library / 3 binary
/ 112 compatibility tests); warnings-denied Clippy, formatting, and
`git diff --check` pass. The service deliberately fails visibly rather than
restarting a broken loop; process restart remains a deployment responsibility.
No public-network request, torrent-count proof, or soak was run.

### TNG-112 — Low-priority RPC circuit-breaker update raced the next request

**Status: Resolved locally with stalled-SCGI regression** · **Priority: P2** · **Confidence: high**

After a background rTorrent RPC timed out, `call_with_priority` used a
fire-and-forget Tokio task to set the 15-second pause deadline. The failed call
could return before that task ran, allowing the next background request to
pass the circuit-breaker check. The timeout path now awaits the deadline write
before returning, while still holding the RPC permit, so subsequent background
calls observe the open breaker.

A local Unix-SCGI regression accepts but does not answer one request, waits for
the client timeout, and immediately issues another background RPC; the second
call is rejected by the open breaker without a second network attempt. The
sidecar locked offline suite passes (235 library / 3 binary / 112 compatibility
tests); warnings-denied Clippy, formatting, and `git diff --check` pass. No
public-network request, torrent-count proof, or soak was run.

### TNG-113 — Live-speed cache writer followed a predictable temporary symlink

**Status: Resolved locally with symlink regression** · **Priority: P2** · **Confidence: high**

`sync::write_live_speeds` previously wrote to the predictable sibling path
`<target>.tmp` with `std::fs::write`, which follows an existing symlink. When
the configured parent directory is writable by another local process, that
could redirect a stats update into another file writable by the service
account. The writer now creates a UUID-named sibling using `create_new`, writes
the payload, and atomically renames only that newly created file to the target.
It removes its temporary file after a write/rename error.

The Unix regression pre-plants the legacy `<target>.tmp` symlink to a victim
file, performs a live-speed write, and verifies the victim is unchanged, the
target receives the new payload, and the symlink remains untouched. The full
sidecar locked offline suite passes (235 library / 3 binary / 112
compatibility tests), as do warnings-denied Clippy, formatting, and
`git diff --check`. No public-network request, torrent-count proof, or soak
was run.

### TNG-114 — Default rTorrent settings overlay was on a read-only mount

**Status: Resolved locally with deployment and filesystem regressions** · **Priority: P2** · **Confidence: high**

The default Compose profile mounted `/config` read-only while the settings API
defaulted to `/config/rtorrent.rc`. The UI treated an existing parent
directory as proof that the overlay was writable, so it enabled saves that
later failed at the filesystem boundary. The default profile now writes its
overlay to `/var/lib/torrentng/rtorrent-ui-overlay.rc`, which is on the
persistent state volume. The entrypoint creates the file owner-only on first
start and appends an import after the copied user config; configured paths are
validated and symlink overlays are rejected. The UI writability flag now uses
a unique exclusive create/delete probe instead of checking only that the
parent exists. On Unix, atomic overlay replacements are mode `0600`.

Regressions cover successful probe cleanup without changing the existing
overlay, rejection of a read-only parent, owner-only replacement mode, and
the default Compose/entrypoint wiring. The locked offline sidecar suite passes
(238 library / 3 binary / 112 compatibility tests); warnings-denied Clippy,
formatting, all 38 deployment Python tests, shell syntax, and `git diff --check`
pass. No public-network request, torrent-count proof, or soak was
run.

### TNG-115 — Multiply encoded credential query keys bypassed classification

**Status: Resolved locally with redaction and curl-policy regressions** · **Priority: P2** · **Confidence: high**

The sidecar status/log redactor and shared certification curl wrapper decoded
percent-encoded query-key names only once. A name such as `%2570asskey`
therefore did not classify as `passkey` after the first decode, allowing its
value through operator diagnostics and allowing a credential-like URL or GET
field to evade the wrapper's authenticated-request policy. Both classifiers
now decode up to eight layers, stop early when decoding reaches a fixed point,
and normalize punctuation before checking the recognized credential names.

Regressions cover multiply encoded URL and GET-data fields, a secret split
after an empty encoded field across a newline, and encoded sensitive JSON
keys. The locked offline sidecar suite passes (239 library / 3 binary / 112
compatibility tests); warnings-denied Clippy, formatting, all 38 deployment
Python tests, shell syntax, and `git diff --check` pass. No public-network
request, torrent-count proof, or soak was run.

### TNG-116 — Credential-key aliases bypassed curl and log classification

**Status: Resolved locally with URL, GET-data, and diagnostic regressions** · **Priority: P2** · **Confidence: high**

The authenticated curl wrapper recognized only part of the sidecar's
credential-key vocabulary, allowing names such as `pass`, `passwd`,
`torrent_pass`, and `tracker_pass` through URL or GET-data checks. The sidecar
redactor also missed compound names such as `secret_key`/`api_secret`
and aliases including `pwd`, `bearer`, and `api_credential`. URL and GET checks
now share one normalized key matcher; the sidecar matcher covers these aliases
without treating arbitrary words containing `pass` as credentials.

Regressions verify the wrapper rejects sensitive URL and GET-data aliases and
that sidecar text/JSON redaction removes passphrase, secret-key, tracker-pass,
bearer/credential, private-tracker `pid`/`uk`/`sig`, and `pwd` values. The locked offline sidecar suite passes (240 library / 3
binary / 112 compatibility tests); warnings-denied Clippy, formatting, all 38
deployment Python tests, shell syntax, and `git diff --check` pass. No
public-network request, torrent-count proof, or soak was run.

### TNG-117 — Header-style credentials bypassed text-log redaction

**Status: Resolved locally with sidecar redaction regressions** · **Priority: P2** · **Confidence: high**

The shared text redactor handled query-style `key=value` fields and sensitive
JSON keys, but left header-style values such as `Authorization: Bearer ...`,
`X-API-Key: ...`, `Cookie: ...`, and `Set-Cookie: ...` visible. rTorrent log
ingestion also used a separate per-token path, so it could not consistently
apply header context. The shared sanitizer now recognizes common sensitive
header names and suppresses the remainder of that logical line while
preserving following lines; rTorrent ingestion uses the same sanitizer.

Regressions cover multi-token bearer/basic values, API-key and cookie fields,
and preservation of the next line. The locked offline sidecar suite passes
(242 library / 3 binary / 112 compatibility tests); warnings-denied Clippy,
formatting, all 38 deployment Python tests, shell syntax, and `git diff --check`
pass. No public-network request, torrent-count proof, or soak was run.

### TNG-118 — Sensitive-query redaction bypassed path and fragment masking

**Status: Resolved locally with path/query redaction regressions** · **Priority: P2** · **Confidence: high**

When a path-like token contained a recognized sensitive query field, query
redaction returned early and skipped the normal path scrub. That exposed the
request's parent path and could retain a path-embedded passkey; preserving a
fragment could also leak unrelated fragment content. The redactor now masks
the whole path whenever it redacts a credential-bearing query, keeps only the
sanitized query, and drops fragments.

Regressions cover path + query + fragment, fragment-only sensitive values,
and the rTorrent-log projection. The locked offline sidecar suite passes
(242 library / 3 binary / 112 compatibility tests); warnings-denied Clippy,
formatting, all 38 deployment Python tests, shell syntax, and `git diff --check`
pass. No public-network request, torrent-count proof, or soak was run.

### TNG-119 — Special curl header-file sources could hang policy checks

**Status: Resolved locally with deployment-script regressions** · **Priority: P2** · **Confidence: high**

The shared curl policy inspected `-H @path` sources by opening the path before
checking its file type. A FIFO therefore blocked the certification process
indefinitely, and symlink/special inputs could be treated as ordinary
non-sensitive headers instead of failing closed. Header classification now
requires a readable regular non-symlink file; invalid sources take the
sensitive-input path and are rejected before curl is invoked.

The Python regression uses a FIFO with a timeout and a symlink, verifies both
are rejected, and confirms the fake curl executable is not run. All 39
deployment Python tests pass; shell syntax and `git diff --check` pass. No
network, torrent-count proof, or soak was run.

### TNG-120 — Punctuation-obfuscated curl query keys bypassed classification

**Status: Resolved locally with URL and GET-data regressions** · **Priority: P2** · **Confidence: high**

The sidecar removed all non-alphanumeric bytes when normalizing credential
names, but the shell curl policy removed only hyphens and underscores. Keys
such as `p.a.s.s.k.e.y` and `access.t.o.k.e.n` therefore escaped URL and GET
checks. The shell classifier now uses the same ASCII-alphanumeric
normalization, with a fixed `C` locale, before matching credential aliases.

The Python regressions verify punctuation-obfuscated URL and authenticated
GET-data fields are rejected before the fake curl executable runs. All 39
deployment Python tests pass; shell syntax and `git diff --check` pass. No
network, torrent-count proof, or soak was run.

### TNG-121 — Native tracker-message redaction lagged sidecar coverage

**Status: Resolved locally with tracker-message regressions** · **Priority: P2** · **Confidence: high**

The native tracker warning/failure sanitizer used a narrower credential-name
list than the sidecar and curl policy. It missed compound aliases such as
`api_secret`, `secret_key`, `passphrase`, `bearer`, and `api_credential`,
obfuscated punctuation, and header forms such as `Authorization: Bearer ...`.
When query redaction matched inside a path-like token, it also returned before
path masking and could retain the path and fragment. The native sanitizer now
uses matching ASCII-alphanumeric key normalization, masks common header values
through their line boundary, and removes path/fragment context when a
sensitive query is redacted.

Regressions cover the missed aliases, multi-token Authorization/API-key/
Cookie headers, path + query + fragment combinations, and control-character
removal. `rt-tracker` passes all 90 tests and warnings-denied Clippy; workspace
formatting and `git diff --check` pass. No network, torrent-count proof, or
soak was run.

### TNG-122 — Query redaction could retain a trailing diagnostic fragment

**Status: Resolved locally with native and sidecar regressions** · **Priority: P2** · **Confidence: high**

For a non-path token such as `announce?pid=secret&x=visible#opaque`, both
redactors replaced the sensitive field but retained the trailing fragment
because the later `x` parameter was ordinary. Fragments are not part of the
request to the remote service, but they can still contain local diagnostic
secrets. Both redactors now drop fragments whenever sensitive query
redaction changes a token, while preserving safe query fields.

The sidecar and `rt-tracker` regressions verify the sensitive value and
fragment disappear while `x=visible` remains. Full locked offline workspace
tests and sidecar tests pass (`rt-tracker`: 90; sidecar: 242 library / 3
binary / 112 compatibility); warnings-denied workspace and sidecar Clippy,
formatting, all 39 deployment Python tests, shell syntax, and `git diff --check`
pass. No network, torrent-count proof, or soak was run.

### TNG-123 — Escaped controls could split credential names before classification

**Status: Resolved locally with sidecar and native tracker regressions** · **Priority: P2** · **Confidence: high**

Control and Unicode formatting characters were being escaped or replaced
before credential-key normalization. Escaped forms such as `\\u{202e}` and
`\\t` added ASCII characters to the key, while line breaks split `passphrase`
or `Authorization` into separate tokens; these paths could leave credential
values visible. Both redactors now recognize escaped control representations
and classify keys and header names across multi-line seams. Bounded recognized
fields preserve ordinary following lines; ambiguous line chains exceeding the
256-byte lookahead fail closed by masking the remaining message. Existing
escaped output is preserved in Sidecar logs, including C0/C1, bidi, and
extended Unicode format controls.

Regressions cover bidi, zero-width, tab, Unicode tag-format, multi-line
credential aliases, split header names, overlong ambiguous chains, and
following-line preservation. The
full locked offline workspace suite passed; after the final line/header
refinements, `rt-tracker` passes all 90 tests and Sidecar passes 242 library /
3 binary / 112 compatibility tests. Warnings-denied `rt-tracker` and Sidecar
Clippy, 39 deployment Python tests, formatting, Bash syntax, and
`git diff --check` pass. The broad workspace test command also ran existing
local scale unit tests; those results are not treated as dedicated capacity
qualification. No public-network test or soak was run.

### TNG-124 — No-follow HTTP redirects could still be accepted as success

**Status: Resolved locally with a loopback redirect regression** · **Priority: P2** · **Confidence: high**

Sidecar egress clients disabled automatic redirect following, but the response
paths relied on `error_for_status()`, which rejects 4xx/5xx and allows 3xx.
Consequently, a workflow webhook could report success after receiving a 302,
and a remote-torrent fetch could pass the redirect response body downstream as
if it were a torrent. A shared status gate now requires a 2xx response before
either bounded body reader accepts it; errors report only the status and
context, not the request URL.

The loopback regression returns a 302 with a separate local redirect target and
verifies the webhook fails, the target is never contacted, and query credentials
are absent from the error chain. No public-network request, torrent-count proof,
or soak was run.

### TNG-125 — Configured backend API clients followed redirects

**Status: Resolved locally with per-adapter loopback regressions** · **Priority: P2** · **Confidence: high**

The qBittorrent, Deluge, Transmission, and TorrentNG adapter clients used
reqwest's default redirect policy. The clients target explicit API/RPC
endpoints, so following an upstream `Location` could send an authenticated
operation to an unconfigured URL; a POST redirect could also change the method
and still end at a 2xx response. Some TorrentNG mutations used
`error_for_status()` directly, which accepts 3xx even when redirects are not
followed.

All four adapters now share an explicitly no-redirect client builder. The
bounded response reader and TorrentNG's direct mutation responses require a
2xx status. Loopback regressions exercise each adapter with a 302 and a separate
trap target, asserting the call errors and the target receives no request. No
external backend or public-network request was used.

### TNG-126 — Public login and trusted-proxy authentication bypassed CSRF checks

**Status: Resolved locally with loopback HTTP/WebSocket regressions** · **Priority: P2** · **Confidence: high**

When API tokens were configured, an unauthenticated request to a public login
route reached the login handler without the same-origin check used by
cookie-authenticated mutations. A loopback regression reproduced a cross-origin
login returning 200 and issuing session cookies. Separately, the trusted
`X-Remote-User` authentication branch returned before the cookie CSRF check, so
proxy-authenticated browser mutations and WebSocket handshakes did not require
same-origin evidence.

Browser-marked public login/logout requests now use the same origin validation;
headerless non-browser login clients remain compatible. Trusted-proxy identity
now gets the cookie-equivalent mutation and WebSocket checks. When Fetch
Metadata is present, only `Sec-Fetch-Site: same-origin` passes; `same-site` is
rejected even if the authority matches. Explicit bearer authentication is
evaluated first and remains exempt as non-ambient authentication. Regressions
verify cross-origin login issues no cookies, same-origin login works,
proxy-authenticated cross-origin mutation and WebSocket requests fail,
same-origin requests pass, and bearer mutation remains available. No external
network was used.

### TNG-127 — Invalid legacy session cookie shadowed a valid SID cookie

**Status: Resolved locally with a compatibility HTTP regression** · **Priority: P3** · **Confidence: high**

In legacy unsigned-session mode, the cookie parser returned the first
`tng_session` or `SID` value without checking it against configured API tokens.
An invalid/stale `tng_session` placed before a valid `SID` therefore caused a
401 instead of allowing the later valid credential. This is an availability
and compatibility failure, not an authentication bypass. Unsigned sessions
are limited to loopback listeners; public listeners require a signing secret.

The parser now accepts an unsigned cookie candidate only after a
constant-time comparison with configured tokens. An invalid candidate returns
`None` from that cookie-part check, allowing parsing to continue to a later
valid cookie. The regression logs in to obtain a valid `SID`, sends it after a
stale `tng_session`, and verifies that the protected request succeeds. No
public-network request was used.

### TNG-128 — Unauthenticated login attempts were unlimited

**Status: Resolved locally with middleware and compatibility regressions** · **Priority: P2** · **Confidence: high**

With API tokens configured, the public login routes compared submitted
credentials in constant time but accepted an unlimited number of guesses. The
auth middleware now limits unauthenticated POSTs to the exact v1, qBittorrent,
and canonical qBittorrent login routes to 10 attempts per TCP peer per
60-second window. Excess attempts return 429 with `Retry-After`; successful
token login clears the peer's counter. Other authenticated requests and
no-token loopback mode do not consume this login budget.

The limiter uses the accepted socket peer address rather than an
untrusted forwarding header, tracks at most 4,096 peer buckets, and evicts
expired or oldest buckets under address churn. Regression coverage verifies
the limit and response header, success reset, independent peers, window
expiry, bounded tracking, and exact route matching. The WebUI now reports the
retry delay instead of mislabeling a 429 as invalid credentials. When a
reverse proxy is the TCP peer, its clients share one application bucket; the
deployment docs recommend an edge limit when separate proxy-client buckets
are needed. No public-network request was used.

### TNG-129 — Peer-ingress failure counters were not exported

**Status: Resolved locally with native metrics and loopback regressions** · **Priority: P2** · **Confidence: high**

`PeerIngressBudget` already counted admitted handshake slots and global/per-IP
budget rejections, but its snapshot was only consumed by tests. The listener
logged global peer-cap rejection, handshake read errors, timeouts, and
malformed handshakes without exposing counters to operators. The shared budget
now also counts those outcomes, `EngineHandle::peer_ingress_stats()` exposes a
snapshot, and TorrentNG-client `/metrics` publishes admitted, global/per-IP
budget rejection, global peer-cap rejection, read-error, timeout, and malformed
handshake counters. Metrics contain no peer-address labels. Regressions cover
budget snapshots, TCP timeout and malformed-wire paths, the EngineHandle seam,
and Prometheus output. No public-network request was used.

### TNG-130 — Historical handoff mixed completed work with stale next steps

**Status: Resolved locally by reconciling the handoff to the canonical ledger** · **Priority: P3** · **Confidence: high**

`NEXT_AGENT_HARDENING_HANDOFF.md` identified itself as historical at the top,
but later sections still claimed tests had not been run and storage-authority,
egress, peer-ingress, and packed-peer-state wiring remained undone. Those
statements contradicted the current source and dated burn-down evidence. The
handoff now dates its current-disposition note to 2026-09-21 and marks the old
branch caveats and implementation order as superseded, while pointing new work
to the canonical audit. No code behavior changed.

### TNG-131 — Failed batched persistence could commit partial writes

**Status: Resolved locally with atomic per-command savepoints** · **Priority: P2** · **Confidence: high**

`ExecuteBatched` caught a command error or panic but still committed the shared
transaction. A command that wrote one row and then returned an error (or
panicked) therefore reported failure while leaving that partial write durable
beside successful siblings. The regression reproduced both cases before the
fix. Every batched command now executes in a worker-generated savepoint;
failure/panic rolls back only that command, preserving successful siblings.
If savepoint creation/rollback/release fails, the worker aborts the outer
transaction and fails the batch. Current production batched callsites use a
single runtime-row update, but the generic worker boundary now enforces the
per-command failure contract. A deferred foreign-key commit regression also
checks whole-batch rollback on commit failure.

### TNG-132 — Native database-worker saturation and latency were invisible

**Status: Resolved locally with bounded, label-free `/metrics` counters** · **Priority: P2** · **Confidence: high**

The native database worker exposed only a healthy/unhealthy gauge, so queue
saturation, command failures, enqueue timeouts, and persistence latency could
not be distinguished operationally. The worker now snapshots queue depth and
capacity, admitted/completed/failed/cancelled commands, queue-admission
timeouts, cumulative queue wait and command latency, and batched transaction
attempt/failure/duration totals. `EngineHandle` reads the atomics directly,
without placing another request on the potentially saturated actor queue, and
`/metrics` exports the values with no operation, torrent, or peer labels.
Duration totals support average calculations; no histogram or tail-percentile
claim is made. Regressions cover a full queue, admission timeout, command
success/failure, deferred commit failure, and Prometheus rendering.

### TNG-133 — Primary scheduler file cache bypassed the shared fd-budget policy

**Status: Resolved locally for retained entries; active leases are separately bounded per cache by TNG-135 and TNG-136** · **Priority: P2** · **Confidence: high**

The engine constructs `MountScheduler::new_for_path`, whose shared `FilePool`
was using a separate 75%-of-current-soft-limit calculation. The 60% helper and
best-effort soft-limit raise were only applied by `StorageRuntime`'s distinct
cache, so fixing that helper alone did not govern the primary engine path. The
standalone cache also treated `RLIM_INFINITY` as an effectively enormous
capacity. Both production cache constructors now use one lazily computed
per-cache ceiling: 60% of the soft limit after the best-effort raise, capped at
65,536 entries. A zero cache capacity remains usable for uncached opens.

The cache ceiling governs retained entries; TNG-135 and TNG-136 separately
bound cached and in-flight descriptors in the scheduler and runtime caches
with lifecycle leases. These limits do not establish a total process-fd
guarantee across distinct caches and unrelated descriptors. Regressions cover
exact low-limit arithmetic, the unlimited ceiling, scheduler capacity wiring,
and zero-capacity cache behavior.

### TNG-134 — Checkpoint sync opened every dirty file at once

**Status: Resolved locally by sequential path sync** · **Priority: P2** · **Confidence: high**

`sync_all_open_files` collected every cached writable handle plus every dirty
path into a vector of live `Arc<File>` values before syncing any of them. The
dirty set can greatly exceed the cache size, so checkpointing a multi-file
payload could consume the remaining process descriptors and fail with
`EMFILE`. A child-process regression lowered both `RLIMIT_NOFILE` values to 64,
wrote 96 dirty files through a one-entry cache, and reproduced the failure
before this change.

The file pool now snapshots cached writable paths rather than handles, merges
them with the dirty-path snapshot, and opens/fdatasyncs each path sequentially.
Dirty generations are cleared only after the entire snapshot sync succeeds;
concurrent newer writes remain dirty. The focused regression now succeeds at
the same low descriptor limit. This removes the checkpoint path's unbounded
temporary handle vector; TNG-135 and TNG-136 separately add per-cache leases
for ordinary in-flight I/O, without claiming an aggregate process-wide limit.

### TNG-135 — In-flight scheduler handles can outlive cache eviction

**Status: Resolved locally per path-backed scheduler `FilePool`; managed caches now share the TNG-138 quota** · **Priority: P2** · **Confidence: high**

`FilePool` now attaches a descriptor permit to each opened file. The permit
remains held while the descriptor is in the cache or referenced by a caller or
backend job, and the `File` is dropped before its permit is released. New opens
wait for capacity, evict cached entries, and retry when operation ownership
changes. The backend job (including a pending `io_uring` completion) owns the
lease until the operation completes even when its awaiting caller is cancelled.
Dropping an operation handle wakes waiters so they can evict a now-idle cached
entry rather than sleep indefinitely.

The local bound is per `FilePool`: cached plus in-flight descriptors cannot
exceed its configured capacity (a zero-entry pool permits one transient open).
`FilePoolStats.open_files` still reports retained cache entries only. This
per-cache bound is supplemented by TNG-138's shared managed-storage budget
across scheduler and runtime caches. Descriptors opened outside `rt-storage`
remain outside that quota.

Regressions cover blocked admission until an evicted in-flight lease drops,
caller cancellation while a pread job remains queued, pending `io_uring` job
ownership through completion, and 72 concurrent distinct-path opens in a child
process constrained to `RLIMIT_NOFILE` at most 64 with a pool capacity of eight. The
pool completes all opens while staying within its per-pool active-lease cap.

### TNG-136 — `StorageRuntime` cache did not account for backend-owned descriptors

**Status: Resolved locally per `StorageRuntime::HandleCache`; managed caches now share the TNG-138 quota** · **Priority: P2** · **Confidence: high**

`HandleCache::get_or_open` previously returned an `OpenFile` whose `file()`
method cloned a raw `Arc<File>`. `StorageRuntime::read_frame` and `write_at`
passed that clone to the disk backend, so an in-flight or queued job could
retain the OS descriptor after LRU eviction while new misses continued to
open files. This was a separate path from the scheduler `FilePool` fixed by
TNG-135.

The runtime cache now reserves a per-cache descriptor permit before opening a
file and keeps it with the cached file and operation handles. The runtime
passes a lease-aware handle into backend jobs, preserving it through caller
cancellation and pending `io_uring` completion. Its async cache-open path waits
on descriptor availability without blocking the Tokio executor; the synchronous
cache API uses the condition-variable waiter. `OpenFile::file()` now borrows
the file, so callers cannot detach an unleased `Arc<File>` through that API.

Regressions cover zero-capacity permit release, wait/retry after an in-flight
evicted handle drops, executor progress while async admission is blocked,
queued-job cancellation and pending-uring ownership using `HandleCache`, and 72
concurrent distinct-path opens under child-process `RLIMIT_NOFILE` at most 64
with a capacity of eight. Its local cache cap is separate from each scheduler
pool's cap, but both draw from TNG-138's shared managed-storage quota;
unrelated process descriptors remain outside that quota.

### TNG-137 — Shared storage metrics were summed once per torrent

**Status: Resolved locally; metrics remain per managed cache/resource set** · **Priority: P2** · **Confidence: high**

Path-backed schedulers with matching I/O configuration share one
`SharedSchedulerResources` instance. `EngineStats` previously summed that
instance's file-pool counters and I/O/hash queue depths once for every torrent
runtime snapshot, inflating shared gauges and counters with torrent count.
Production stats collectors now track shared resource identities for each
snapshot, report the latest observed gauges once per unique resource set, and
retain the maximum cumulative cache counters observed for that set.
Per-torrent operation counters continue to sum. Snapshots with an unknown
identity are left additive rather than silently merged.

The `open_files` field continues to mean retained cache entries; it no longer
stands in for every live descriptor. `FilePoolStats.active_descriptors` counts
the cache, open-attempt, and operation leases, and `/metrics` exports separate cached
entry and active-descriptor gauges for both the scheduler `FilePool` and the
independent `StorageRuntime::HandleCache`. Shared scheduler cache memory is
also excluded from per-torrent hot-memory estimates, avoiding attribution of
the entire common cache to every torrent; its aggregate gauge remains
available. Regression coverage verifies that two torrents sharing an
identity update one shared snapshot without losing a newer gauge or cache
counter, distinct resource identities still add, shared cache memory is not
multiplied in per-torrent estimates, and per-torrent I/O counters still sum.

The pool gauges describe unique resource sets. TNG-138 additionally exposes
aggregate lease use and capacity across all managed storage caches; unrelated
process descriptors remain outside that storage quota.

### TNG-138 — Per-cache descriptor ceilings exceeded the intended daemon budget

**Status: Resolved locally for managed storage caches; unrelated descriptors remain outside the quota** · **Priority: P1** · **Confidence: high**

Each `DescriptorLimiter` previously enforced its own 60%-of-`RLIMIT_NOFILE`
ceiling. The path-backed scheduler and `StorageRuntime::HandleCache` could
therefore each admit up to that amount independently, and differently
configured scheduler pools multiplied the allowance further. This contradicted
the intended socket reserve and the storage handoff's requirement for one
daemon-level descriptor budget.

All cache limiters now reserve from a single process-level managed-storage
gate in addition to their local caps. Admission and release acquire the global
gate before the local gate in one lock order. A release signals sync and Tokio
waiters across cache instances, so pressure in one cache wakes a blocked
request in another. Zero-capacity local caches retain their existing one-open
transient behavior, still subject to the process quota.

`/metrics` now exports aggregate active descriptor leases, shared quota
capacity, and admission attempts that encountered global backpressure, while
the per-cache gauges remain available. A low-`RLIMIT_NOFILE` child-process
regression fills the shared quota across two independent cache limiters,
verifies further admissions wait, and confirms a released slot can be acquired
by the other cache. This is a budget for `rt-storage` leases, not a hard cap on
sockets or other descriptors opened outside the storage subsystem.

### TNG-139 — IPv6 DHT pending-forward state bypassed aggregate admission

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The IPv4 DHT path rejected a new pending-forward torrent when either the
global retained-peer cap or the pending-torrent cap was full. The IPv6 path
entered `pending_peer_forwards` before checking either condition. Once the
global peer cap was full, each new IPv6 info-hash could therefore leave an
empty map entry behind, and a stream of distinct hashes could grow the map
beyond the intended pending-torrent bound.

The IPv6 queue now applies the same pre-entry checks as IPv4, while existing
pending torrents may still append only within the per-torrent/global peer
limits. Regression coverage fills the global cap, verifies an existing entry
cannot grow, and verifies a new IPv6 hash is not inserted as an empty entry.

### TNG-140 — Shared CSRF Fetch Metadata accepted non-same-origin claims

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The shared native/qBittorrent CSRF helper rejected `cross-site` but treated
other present `Sec-Fetch-Site` values as acceptable. That admitted
`same-site`, `none`, unknown, and malformed metadata when `Origin` and
`Referer` were absent, even though the helper's contract said an explicit
Fetch Metadata claim must fail closed. A sibling origin could therefore reach
cookie-backed mutations in configurations relying on this shared helper.

The helper now requires a valid, case-insensitive `same-origin` value whenever
`Sec-Fetch-Site` is present. Missing Fetch Metadata retains the existing
Origin/Referer and non-browser compatibility behavior. Regression coverage
checks same-site and invalid metadata in addition to the existing CSRF matrix.

### TNG-141 — Facade Bearer parsing disagreed with the daemon guard

**Status: Resolved locally** · **Priority: P2** · **Confidence: high**

The daemon-level authorization guard accepted the HTTP authentication scheme
case-insensitively, while inner native, qBittorrent, Transmission, Deluge, and
sidecar guards used a literal `Bearer ` prefix. A valid `bearer <token>` or
mixed-case scheme could therefore pass the outer guard and be rejected by the
selected facade, producing inconsistent authentication behavior.

The Rust facades now use one shared parser that requires exactly two
whitespace-separated fields, compares the scheme case-insensitively, and
returns only the credential. Sidecar applies the same rules locally. Tests
cover lower-case/mixed-case schemes and reject extra fields.

### TNG-142 — Auth middleware public exceptions used suffix matching

**Status: Resolved locally** · **Priority: P1** · **Confidence: high**

The qBittorrent facade classified any path ending in `/auth/login` or
`/auth/logout` as public, and the native idempotency middleware used the same
suffix test when exempting login/logout requests. The currently registered
routes do not expose state at arbitrary nested paths, but this made the
authorization contract broader than the route table and would silently expose
any future matching nested route.

Both exceptions now match only the exact registered paths. Regression coverage
rejects nested, suffixed, and unrelated paths while retaining the four
qBittorrent and two native auth routes.

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

The shared address policy also gates DHT bootstrap/routing destinations and
peer endpoints returned through DHT, trackers, PEX, and manual peer commands in
the v1/hybrid and pure-v2 task paths. A follow-up found that metadata-pending
magnets have a separate TCP/uTP connector before a full torrent task exists;
that path now filters candidates before retry-state admission and validates
again immediately before acquiring a peer connection budget or opening either
transport. This covers BEP 9 `x.pe`, tracker, DHT, and forwarded metadata-peer
addresses. Loopback, private, and link-local traffic remains opt-in through the
existing `[tracker]` egress switches.

The metadata-pending actor also shares the authoritative session registry for
global peer bans. It removes banned endpoints before retry-state admission,
checks again before opening TCP/uTP, and cancels an active fetch when the
engine broadcasts ban eviction.

Focused tests cover denied metadata candidates without retry-cache pollution,
denial before a loopback TCP connect, banned-peer eviction without reconnect,
IPv4/IPv6 loopback DHT traffic with and without explicit opt-in,
private-address classification, allowed public destinations, redirect policy,
body limits, and DNS address validation. Public-network hostile DNS/redirect
behavior and interoperability remain evidence work; the implementation
boundaries are covered by local tests.

### TNG-006 — Authentication is fail-open and inconsistent across facades

**Status: Functional implementation complete; evidence deferred** · **Priority: P0** · **Confidence: high**

Verified evidence: TorrentNG, qBittorrent, Transmission, and Deluge mounted
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

Full TorrentNG-client workspace tests, compatible-client service tests, format, compile, and strict clippy are
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

Follow-up verified 2026-09-20: every persisted global-limit update reapplied
the effective value to `SharedRateLimiter::set_limit`, which reset the bucket
to a full burst even when the effective limit had not changed. Repeatedly
saving the same limit therefore bypassed the intended pacing burst. The setter
now leaves bucket state untouched for an unchanged effective limit; paused-
clock regression `reapplying_same_limit_does_not_restore_spent_burst_tokens`
proves an exhausted bucket still waits for refill. The focused test passes;
multi-peer fairness and wire-byte measurements remain deferred.

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
The older production-daemon scale report records 100k restore, TorrentNG/qBit
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
accumulate as unbounded supervisor waiters. TorrentNG-client save-path moves return
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

Verified evidence (2026-09-03): the TorrentNG list API returns a bounded page and
immutable revision cursor; snapshots are cached for 750 ms, sort indexes are
lazy and shared, refreshes are single-flight, and an expired cursor returns
`410 Gone`. TorrentNG SSE sends one or more bounded initial snapshot chunks
followed by mutation-journal deltas and performs a bounded snapshot resync
only when the bounded journal expires.
When the journal still covers the cached generation, TorrentNG and qBittorrent
snapshot refreshes apply only the changed hashes; they fall back to a registry
scan when there is no usable base snapshot or the journal has expired.
Engine stats uses a 500 ms cache, parallel task queries (up to 64), a 250 ms
aggregate task-query deadline, and per-query timeouts; TorrentNG and qBittorrent
transfer-info now consume its aggregate rate/byte snapshot instead of walking
every torrent actor.
Durable torrent totals, byte totals, activity-tier counts, tracker status
counts, and active-job counts use maintained counters or aggregate SQL rather
than materializing every matching row.
The TorrentNG qBittorrent facade's `/torrents/info` has the same pinned snapshot
and page index, returns `X-TorrentNG-Snapshot`, and `/sync/maindata` uses the
registry journal for changed/removed torrents. The compatible-client service's
TorrentNG backend now carries that TorrentNG snapshot token through every bounded
sync page and resilient sub-range retry; its qBittorrent backend also uses bounded,
hash-sorted `torrents/info` pages, but that external API has no server-side
snapshot, so its view is explicitly eventual and cleanup is skipped for a
cycle with page faults. The compatible-client service's qBittorrent compatibility
`/sync/maindata` path now rejects full or incremental responses over 10,000
torrents with `413` instead of silently truncating a full sync or materializing
an unbounded delta. In compatible-client service mode that compatibility cursor is now a
durable SQLite revision with bounded deletion tombstones; wall-clock seconds
are not used, so same-second updates and removals cannot disappear between
polls. Large qBit projections skip
per-torrent live engine round-trips; durable fields and aggregate stats remain
available. TorrentNG SSE initial state is emitted as bounded chunks (default 500,
maximum 1,000) at one revision, with `snapshot_complete` framing; subsequent
events remain journal deltas.
qBittorrent `/log/peers` now queries only promoted runtime tasks, in parallel
with a bounded per-task deadline; dormant rows have no live peers and are not
walked one by one.

The redesign does not make every operation sublinear. A journal-driven refresh
still clones the immutable snapshot and rebuilds its filter indexes, TorrentNG
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
[`interop-matrix-20260910T190228Z.md`](../certification/reports/interop-matrix-20260910T190228Z.md);
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
not pretend that client-side slicing is server-side pagination. TorrentNG and
qBittorrent endpoints remain the paged/snapshot-capable path.

The current implementation and local process-load gate is complete: the snapshot/index contract,
bounded SSE framing, cursor expiry, journal resync, and deliberately omitted
large-qBittorrent live fields are documented and covered by source-level
tests. TorrentNG sidebar media facets now use the same incremental snapshot index
instead of rescanning the snapshot; compatible-client service hot read paths use a
bounded blocking-DB gate, and its log/stats probes keep filesystem reads behind
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

### TNG-016 — Pure v2 completion is a bounded capability

**Status: Implemented locally for complete metainfo and `btmh` magnet completion; external evidence deferred** · **Priority: P1** · **Confidence: high**

The native engine now starts a dedicated pure-v2 actor for complete `.torrent`
or raw metainfo. It restores v2 file paths and priorities, rechecks file roots,
handles partial resume and seeding, serves and downloads BEP 52 pieces over TCP
and uTP, performs bounded hash exchange with `hash reject` responses for
unsupported ranges, and runs the v2 tracker announce/update/reannounce
lifecycle using the truncated v2 infohash required by the tracker protocol.
Hybrid BEP 47 padding is treated as synthetic zero content and is not required
on disk. Pure-v2 file trees use BEP 52 alignment gaps between non-empty files;
they do not require a materialized padding file.

Pure-v2 `btmh` magnet metadata completion now obtains the exact BEP 9 info
dictionary, verifies the full v2 SHA-256 identity, acquires required BEP 52
piece layers with bounded hash exchange and Merkle-proof validation, and
promotes only verified metainfo. The capability manifest reports
`pure_v2_metadata_completion: true`; public-network interoperability,
real-client coverage, and target-hardware evidence remain external gates.

Acceptance: focused v2 recheck, path-policy, peer-wire, hash-exchange,
padding, tracker, uTP, BEP 9 metadata, direct-peer, and engine-promotion tests
pass; public-client/device/soak evidence remains deferred.

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

**Status: Functional implementation complete for IPv4 and IPv6; evidence deferred** · **Priority: P1** · **Confidence: high**

Original evidence: the live task was IPv4-only; there was no effective rate
limit or outstanding expiry; transaction IDs were two bytes; response source
validation and global announced-peer caps were incomplete.

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

The implementation gap is now narrowed to external proof. The live DHT task
binds and routes IPv4 and IPv6 packets, validates response source addresses
and tokens, bounds inbound work globally and per source IP, caps tracked
torrents/query history/outstanding requests/announced peer sets, and expires
stale outstanding work. IPv6 compact peer values are forwarded to the same
source-aware peer admission path. Focused tests cover both address families,
spoofed responses, transaction handling, timeout expiry, per-IP/global flood
budgets, global announced-peer caps, and token validation.

The task now additionally applies the shared outbound address policy to IPv4
and IPv6 bootstrap targets, routing-table destinations and additions, DHT
response sources, and peer values forwarded to torrent tasks. Metadata-pending
magnets and full v1/hybrid/pure-v2 transfers apply that policy again at their
own TCP/uTP admission boundary, so DHT discovery cannot turn a denied address
into a connection attempt. Private-network DHT and peer operation requires the
corresponding explicit egress opt-in.

Remaining action: run broader hostile-input/load and restart evidence.

Acceptance for the implementation gate is met; live flood measurements and
restart evidence remain deferred.

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

### TNG-021 — TorrentNG list API does not match its documentation

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
applied to the TorrentNG client; several operator-facing stores remain
process-memory state.

Verified evidence (this session): a targeted audit (not the full
method-by-method matrix the acceptance criteria calls for) found and fixed
the two highest-confidence, easiest-to-fix inert mutations -- both had an
already-working TorrentNG-client method one facade over, just never wired to
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
categories and global tags in the TorrentNG-client database, restores them across client
restart, persists peer bans, restores bans before listeners start, and evicts
banned peers from active tasks on the tracker reconciliation path. TorrentNG-client
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
compatibility defaults, and only TorrentNG-client Label/Notifications appear enabled.

The remaining action is a real-client compatibility matrix covering the
documented projection-only surfaces and unsupported responses. Move-on-
completion and blocklist/plugin behavior are not claimed as TorrentNG-client features.

### TNG-023 — Capability and health manifests overclaim implementation

**Status: Functional implementation complete; certification evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence: the TorrentNG-client capability manifest now separates
`implemented`, `enabled`, `certified`, and `experimental` assurance states.
Runtime/config-dependent uTP fields are derived from active policy, while
scale certification remains outside the advertised implemented/certified set.
Pure-v2 metadata completion is now
advertised as implemented after exact infohash and piece-layer proof
validation. Pure-v2 transfer and IPv4/IPv6 live-DHT routing remain locally
implemented but not certified; certification is still governed by accepted
external evidence. Contract tests cover the manifest shape and mounted routes.

The remaining action is to keep `certified` empty for capabilities without
accepted external evidence and to update it only from a release/evidence
review, not from a local unit-test pass.

### TNG-024 — Deployment defaults are unsafe, inconsistent, or silently ignored

**Status: Functional implementation complete; deployment evidence deferred** · **Priority: P0/P1** · **Confidence: high**

Verified evidence: `Config::validate()` (called from the real config-load
path, `rt-config/src/lib.rs`) now unconditionally rejects placeholder tokens
and requires non-empty tokens of at least 16 characters for public binds.
Existing invalid config files no longer silently fall back to defaults;
defaults apply only when no config file exists. TorrentNG-client deployment templates
use the declared peer port, Docker builds use `--locked`, and rendered Compose
configuration validates locally. The compatible-client and Phase 1 images now
run as configurable nonzero UID/GID, reject UID 0 during identity setup, and
mount user configuration read-only. Their writable runtime roots are prepared
by a short setup phase and the service then runs as the selected `PUID`/`PGID`;
Compose passes matching values to the runtime and optional LinuxServer
backends. Phase 1 nginx listens on
unprivileged container port 8080. Existing root-owned volumes have an explicit
one-time migration command in `docs/DEPLOYMENT.md`.

Remaining action is applying and checking that migration on an existing
deployment, verifying custom host-mount permissions on the target storage,
and target-orchestrator rendered-secret review. The checked-in Kubernetes
secret remains a template and must be populated by the operator before apply.

## P1/P2 — release evidence and engineering system

### TNG-025 — CI does not enforce both runtime quality gates

**Status: Repository gate resolved; branch-protection review outstanding** · **Priority: P1** · **Confidence: high**

Verified evidence: `.github/workflows/ci.yml` gained a `native-quality` job
(the job id is retained; it is displayed as TorrentNG client quality)
(fmt check, OpenAPI validation, `cargo test --workspace --all-targets
--locked`, `clippy -D warnings`) plus formatting, tests, and clippy for the
compatible-client service (was build-only before). `.github/workflows/release.yml` got the same
combined TorrentNG-client/compatible-client service gate plus an authenticated release-binary smoke using
the tracked `certification/fixtures/backend-burndown-native-release-smoke.toml`
fixture. Both first-party binary and Linux asset jobs require the quality,
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
exact declared floor (`1.88.0` main, `1.97.0` compatible-client service) via `@1.88.0`/
`@1.97.0` version tags, and running a real build + full test suite at
that exact version -- alongside, not replacing, the existing `@stable`
TorrentNG-client/compatible-client-service jobs (which still track current/future stable,
a distinct and still-valuable check). Verified both jobs' exact commands
locally against the already-installed `1.88` and `1.97.0` rustup
toolchains before committing: `cargo +1.88 build/test --workspace
--all-targets --locked` green, `cargo +1.97.0 build/test --locked
--manifest-path sidecar/Cargo.toml` green (75 compatible-client service tests passed).

Hosted evidence is now present: CI run `33915548520` passed all ten jobs on
`f1c39fd`, including TorrentNG-client quality, both MSRV jobs, fuzz smoke, compatible-client service,
WebUI, dependency security, backup/restore, API/SSE load, and fault
containment. The dynamic `Push on main` orchestration also passed as run
`33915547352`. The remaining repository action is settings review: GitHub
branch protection is not evidenced as requiring these jobs.

Follow-up source review found that both RustSec jobs audited only the root
lockfile even though the compatible-client service and fuzz harness have
separate lockfiles. The CI and release workflows now invoke `cargo audit
--file` for all three; `scripts/security_scan.sh` reports each audit
separately and uses a private temporary directory rather than shared fixed
`/tmp` filenames. The local scan does not claim a RustSec result without the
`cargo-audit` executable.

### TNG-026 — Release evidence is stale or weaker than its claims

**Status: Local and hosted repository evidence current; external evidence deferred** · **Priority: P1** · **Confidence: high**

Verified evidence (2026-09-04 UTC):
`target/release/torrentngd` was rebuilt from the clean `main` tree at commit
`83b70ce` with `cargo build --release --locked -p torrentngd`, launched with
an isolated authenticated config, exercised through health, TorrentNG
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
shared idempotency claim/replay/conflict tests cover the TorrentNG client, qBittorrent,
Transmission, and Deluge mutation routers. A broader parser and mutation
replay corpus remains optional evidence work.

### TNG-028 — Formatting, clippy, and MSRV are already red

**Status: Repository gate resolved; branch-protection review outstanding** · **Priority: P1** · **Confidence: high**

Verified locally (see "Current verified evidence" above for full detail):
`cargo fmt --all -- --check`, `cargo test --workspace --all-targets
--locked`, and `cargo clippy --workspace --all-targets --locked -- -D
warnings` all pass now, including on the actual declared MSRV toolchains
(1.88 main workspace, 1.97 compatible-client service -- both `rust-version` fields were
corrected from an untrue "1.80" to the real, verified floor). Two clippy
findings were fixed (too-many-arguments on an egress-policy-widened
function, a redundant `u32 -> u32` cast). The TorrentNG-client quality, MSRV, and
compatible-client service checks are defined in CI and pass in hosted run `34521941751`.
Repository branch-protection enforcement remains a settings review, not a
source-code gap.

## P1 — sidecar API correctness and request resource control

### TNG-030 — Torrent API paths trust backend no-ops and bulk fanout is unbounded

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

Before this fix, native sidecar torrent routes delegated mutations and list
requests to a backend before resolving the requested hash against the local
cache. Backends that treat unknown IDs as successful no-ops could return false
success, while later cache projection could turn the same request into a 500.
Tracker/file reads could similarly return an empty 200 for a nonexistent
torrent. The regression test reproduced these inconsistent statuses across
the single-torrent action, tracker, file, and delete routes.

Single-torrent routes now resolve and pass the cache's canonical hash spelling
before backend access; missing IDs return 404. Bulk actions canonicalize known
IDs, report missing IDs as per-hash errors, and never dispatch those misses.
The backend URL builder also preserves literal `.`/`..` hash segments as data,
preventing URL-join dot-segment normalization from changing the authenticated
endpoint.

The bulk route previously created one Tokio task for every submitted hash and
then used a semaphore only to limit active backend calls. Thus a large request
could allocate an unbounded queue of waiting tasks. The route now caps one
request at 10,000 hashes, caps individual hash/category/path fields, and feeds
a rolling window of 32 tasks. The 10,000 value is an API request resource bound,
not a library-size or deployment-capacity claim. The shared no-follow file
reader also uses nonblocking open semantics so a FIFO at a session-metainfo or
peer-ID state path cannot hang the sidecar before it can reject the file type.

The qBittorrent facade had a related explicit-selection path that first
materialized every pipe-separated ID and only then deduplicated. It now parses
incrementally, rejects more than 10,000 raw entries or a non-ASCII/oversized ID
before building the deduplicated list, and preserves the protocol's
`hashes=all` cache-expansion behavior. Its manual-peer and peer-ban parsers now
also enforce their respective 4,096/65,536 address limits and a 128-byte
per-address bound before allocating the address vector.

Focused coverage includes cache-missing route/bulk behavior, dot-segment URL
joining, bounded native/qBittorrent selection and peer-list validation, and FIFO
rejection without a writer. The current sidecar suite passes 175 unit tests and
94 integration tests;
Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation,
and `git diff --check` pass locally. No public-network, capacity, or soak test
was run for this item.

### TNG-031 — qBittorrent compatibility state and delimited lists were unbounded

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

Several qBittorrent list parsers collected pipe-, comma-, or newline-separated
values before validating them. This affected search plugin lists, categories,
tags, tracker URL lists, file-priority indices, and torrent-add URL lists. Search
plugin records, retained search jobs, RSS item maps, and RSS rule records also
had no total-entry ceiling. Search results without an explicit ID selected the
lexicographically greatest key, so job `9` could be returned instead of job
`10`; repeated `rss/setRule` calls for one name generated new IDs and duplicate
rules. RSS folder/feed handlers kept state only in process memory even though
the API contract said that state was durable.

Delimited values now validate count and per-value bytes as they are consumed,
before building a vector or making backend calls. Torrent-add URL batches and
file-priority indices have explicit count, byte, and numeric-width limits;
torrent-add multipart fields reject duplicates and are capped at 16.
Search plugin state is capped at 256 records with atomic capacity preflight;
job history retains 256 entries and evicts the smallest numeric ID. No-ID search
results now choose the greatest numeric ID. RSS items are stored with atomic
SQLite read-modify-write transactions, capped at 1,024 entries, and bounded by
path/URL length. RSS rules are capped at 1,024, `setRule` updates by name while
retaining the stable ID, and both native and qBittorrent rule ingress validate
field/tag sizes before persistence. API docs now distinguish durable SQLite RSS
state from process-local inert search state.

Focused coverage reproduces numeric latest-job ordering, oldest-job eviction,
plugin capacity atomicity, RSS item persistence and capacity, by-name RSS rule
updates, oversized file-priority and torrent-add lists, duplicate multipart
field rejection, and bounded parser behavior. Final test and lint counts are
recorded in the 2026-09-20 burn-down
entry below. No public-network, torrent-capacity, or soak test was run.

### TNG-032 — Native metadata mutation fields were not byte-bounded

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

The root native API and compatible-client sidecar accepted arbitrarily long
category/tag names and category save paths on create/delete/assignment routes.
The root torrent-tag endpoints reused a generic 16,384-item array parser,
while the sidecar checked per-request arrays but had not protected the root
native paths, torrent-add labels, or bulk tag updates. These values are
persisted or forwarded to a backend and can also enlarge later list/snapshot
responses.

Both API layers now cap category/tag names at 256 UTF-8 bytes and category
save paths at 4,096 bytes. Per-torrent tag changes are limited to 1,024 names
of at most 256 bytes; root PATCH requests also cap the combined add/remove
count. The root JSON model applies the same count and byte bounds to add-torrent
labels, and root bulk/tag routes reject oversized input before resolving or
mutating torrents. OpenAPI now describes these constraints and records the
actual 201 add-torrent response and 200/204 metadata responses.

Regressions cover long category/tag names and paths, overlong path parameters,
per-torrent category assignment, POST/PATCH/DELETE tag mutations, bulk tag
updates, and add-torrent label deserialization. Targeted tests pass for
`rt-api-model` (20) and `rt-api-native` (75); sidecar tests pass (175 unit / 94
integration). Warnings-denied root/sidecar clippy, formatting, OpenAPI
validation, and diff checks pass. No public-network, capacity, or soak test was
run for this item.

### TNG-033 — Sidecar metadata dictionaries and tag projections were unbounded

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

Per-request field limits did not bound the sidecar's persistent `categories`
and `tags` dictionaries. Repeated valid writes could grow them indefinitely,
and list endpoints materialized the complete tables. Tags could also accumulate
across individually valid writes to more than the intended per-torrent bound
or across torrents into a large list/delta response. Backend sync parsed the
resulting tag string into an unbounded vector, while detail/list SQL used
unbounded `GROUP_CONCAT` projections.

The sidecar now caps each category/tag dictionary at 16,384 names and 4 MiB
of retained UTF-8 bytes. Names remain limited to 256 bytes; category paths are
limited to 4,096 bytes. A torrent may have at most 1,024 tags, and backend tag
sync parses at most 1,024 labels from at most 512 KiB of raw input. Total
assignments are capped at 1,000,000 links / 64 MiB of assigned tag text.
SQLite triggers maintain O(1) global and per-torrent summaries, rebuilt from
the authoritative rows at startup. These checks run transactionally at each
cache write. Category/tag writes that preflight as over capacity return `429`
before the corresponding backend mutation; the transaction repeats the check
before commit. Over-limit legacy rows are not truncated: list/delta projections
fail with `503` when aggregate totals exceed the cap, while single-torrent
detail projections remain bounded by their per-torrent SQL limit. qBittorrent
multi-hash writes preflight their combined positive growth before backend calls,
so partial successes do not exceed the aggregate cache limit. Native and
qBittorrent torrent-add multipart text is read incrementally under per-field
byte limits; file data is also streamed into one bounded buffer up to 64 MiB.
Native JSON tag arrays retain at most 1,024 strings of at most 256 bytes while
deserializing, rather than materializing an oversized vector before checking
limits. Multipart metadata is field-count/duplicate checked and category,
save-path, and magnet values are byte-bounded; category capacity is preflighted
before sidecar backend add calls.

Regressions cover exact dictionary/per-torrent/aggregate boundaries, over-limit
legacy names/paths/assignments, trigger summary rebuild and cascade accounting,
combined multi-hash preflight, bounded SQLite tag projections, status mapping,
bounded JSON tag deserialization, API and qB tag-add rejection before a mock
backend call, and malformed/duplicate/oversized multipart add fields. Sidecar test and warnings-denied lint counts are recorded
in the 2026-09-20 verification row below. These are local guard regressions,
not torrent-count, throughput, or soak evidence; those claims remain deferred.

### TNG-034 — Batch request bounds did not constrain aggregate backend fan-out

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

Several individually bounded request fields still multiplied into large work:
native cross-seed hashes and trackers, qBittorrent hash selections combined
with tag/tracker/peer lists, repeated live-stats hashes, and persisted ratio or
workflow rules selecting every matching cache row. In addition, qBittorrent
`hashes=all` expanded the entire local cache, bypassing the explicit 10,000-entry
cap. Ratio-group actions also bypassed the selected backend abstraction by
calling the embedded rTorrent client directly.

Native JSON now caps cross-seed selections at 10,000 hashes (up to 1,024 UTF-8
bytes each) and 1,024 tracker URLs (up to 8,192 bytes each), and rejects
non-dry-run requests above 10,000 combined tracker/reannounce backend
operations. Tracker patch arrays cap each add/remove/edit list at 1,024 entries;
URLs cap at 8,192 bytes. File-priority patches cap at 4,096 entries. Bulk hash
arrays retain at most 10,000 entries and do not retain hash strings longer than
256 characters. Live-stats accepts at most 128 non-empty comma-separated hash
entries, counts duplicates before deduplication, and caps the raw value at
8,320 bytes during query deserialization.

qBittorrent `hashes=all` now queries at most 10,001 rows to detect a selection
above the same 10,000-target limit without loading the full cache. Nested
`addTrackers`, `addTags`, `removeTags`, `setTags`, and `addPeers` requests cap
the torrent-by-item product at 10,000 and return `413` before backend mutation.
The product calculation saturates on integer overflow; an empty `setTags`
replacement still counts one operation per selected torrent.

Ratio-group and workflow selectors now apply SQL `LIMIT 10001`; matches above
10,000 return `413` before dry-run materialization, history writes, or backend
actions. Ratio-group limits now dispatch through the selected backend and return
`501` when share limits are unsupported, matching the compatibility backend
contract rather than always targeting the embedded rTorrent client. Workflow
category filters are now distinct from `target_category`; legacy category-only
set-category rules are normalized to target-only so they do not select only
torrents already in the destination category. RSS test/apply title and link
fields are capped at 8,192 UTF-8 bytes during JSON deserialization so large
samples cannot be repeatedly cloned and lowercased during rule matching.

Regressions cover the live query byte/item limits and duplicate counting,
bounded qB and rule-selector SQL lookahead, cross-product boundary/overflow
cases, integration requests proving oversized qB tracker/tag/peer batches
return `413` with zero backend calls, and rule fan-out rejection with zero calls.
A separate route regression verifies ratio-group mutations reach the selected
backend. Native JSON request-array cases cover item/string caps and the
cross-seed operation limit. No torrent-count proof or soak was run; both remain
deferred to the later testing phase.

### TNG-035 — Persisted automation rules had no storage or field bounds

**Status: Resolved locally with focused regressions** · **Priority: P1** · **Confidence: high**

Ratio groups and workflow rules are stored as JSON arrays in SQLite key/value
rows, but their APIs previously accepted unbounded counts and most workflow
strings had no byte ceilings. Repeated upserts therefore increased both durable
state and the cost of every later list/update operation.

The API now caps each collection at 1,024 entries while allowing updates to
existing ratio-group names and workflow IDs at capacity. New entries return
`429`. Ratio group names/category filters cap at 256 bytes and tracker filters
at 8,192. Workflow fields cap at id 128, name 256, event/action 64,
category/target_category 256, tracker/command/url 8,192, and target path 4,096
bytes. The handler checks these sizes before trimming/copying fields.

Regression coverage seeds both stores at capacity, proves updates still work,
new records return `429`, oversized names return `400`, and collection counts
remain unchanged. This is an application API bound; no torrent-count or soak
evidence is implied.

## P2 — architecture and maintainability

### TNG-029 — The engine has poor fault/change isolation

**Status: Persistence-isolation implementation and local fault evidence complete; broader decomposition deferred** · **Priority: P2** · **Confidence: high**

Verified evidence (2026-09-04): explicit seams now exist for storage-job
dispatch/control/recovery, registry revisions and mutation deltas, TorrentNG and
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
finalizes metadata after payload cleanup. TorrentNG SSE initial snapshots are
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

The TorrentNG client's high-volume session-event writes use a bounded,
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

### TNG-036 — Poisoned mutexes turn isolated panics into persistent failures

**Status: Resolved locally with panic-injection regressions** · **Priority: P2** · **Confidence: high**

`std::sync::Mutex` remains poisoned after a panic. Lock sites that called
`expect` therefore converted one panic into repeat failures on later API,
network, storage, or shutdown operations; cleanup locks could also panic again
while unwinding. This pass covered native and qBittorrent snapshot order
caches, the outbound HTTP-client cache, storage file/handle/read/frame caches,
registered-buffer slots, disk-worker receivers, per-torrent prepared-file
bookkeeping, rate and per-IP admission state, idempotency claims, and owned
engine task handles.

Recovery follows the state authority rather than applying one generic reset:

- Derived indexes, open-handle maps, read-ahead entries, prepared-file
  membership, and idle frame buffers are discarded and rebuilt from their
  immutable snapshot or authoritative filesystem state.
- Registered frame-slot lists drop invalid and duplicate indices but never
  invent a slot that may still be in flight.
- Dirty-write generations, device queue semaphores, queued disk work, token
  balances, per-IP reservations, idempotency in-flight/completed entries, and
  task join handles are preserved. This avoids losing durability work,
  resetting admission limits, replaying a mutation, or detaching owned tasks.
- SQLite worker locks remain fail-closed and return persistence errors; the
  code does not resume a possibly interrupted transaction.

Panic-injection regressions cover cache rebuilds, registered-slot
deduplication, queued receiver work, dirty-path retention, token and peer
admission limits, idempotency replay, prepared-file recovery, and shutdown
ownership. The full locked workspace test suite and warnings-denied lint pass.
These are local containment results; no public-torrent, capacity, deployment,
or soak evidence is implied.

### TNG-037 — Browser-origin checks did not cover cookie-authenticated WebSockets

**Status: Resolved locally with auth regressions** · **Priority: P1** · **Confidence: high**

The sidecar now applies same-origin/CSRF metadata checks to `/ws` as well as
browser mutation methods. Loopback no-token mode still rejects cross-origin or
metadata-free browser requests; API-token authentication retains its separate
bearer-token path. Focused tests cover accepted same-origin and rejected
cross-site/missing-origin cases.

### TNG-038 — Torrent add accepted conflicting source fields

**Status: Resolved locally with request regressions** · **Priority: P2** · **Confidence: high**

Native JSON and multipart requests, sidecar multipart requests, and the
qBittorrent add facade now reject simultaneous torrent and magnet/URL sources
before backend mutation. qBittorrent URL/torrent alternatives are mutually
exclusive, and native/sidecar multipart forms require exactly one source.

### TNG-039 — WebUI ignored the server's full-resync signal

**Status: Resolved locally with a hook regression** · **Priority: P2** · **Confidence: high**

The WebSocket hook now invalidates the full query cache on `resync_required`,
so cached lists and derived projections are refreshed instead of waiting for
an ordinary torrent delta. A hook test covers the resync event.

### TNG-040 — Future timestamps could make live-speed samples appear fresh

**Status: Resolved locally with a boundary regression** · **Priority: P2** · **Confidence: high**

The compatible-client live-speed freshness check allows at most five seconds
of future clock skew. More distant future samples are treated as stale rather
than suppressing valid updates indefinitely.

### TNG-041 — Invalid sync rows could bypass field bounds or delete prior cache data

**Status: Resolved locally with projection regressions** · **Priority: P2** · **Confidence: high**

Each sync-projection field is checked against its byte limit before persistence.
An invalid row is rejected without advancing the projection or deleting the
previous cached row, so malformed remote state cannot silently erase good local
data.

### TNG-042 — FIFO paths could block before bounded readers checked file type

**Status: Resolved locally with filesystem regressions** · **Priority: P2** · **Confidence: high**

Configured config, stats, log, and overlay reads open with Unix `O_NONBLOCK`
and validate regular-file metadata from the opened handle; symlinks to regular
files remain supported for configured paths. Identity/metainfo reads use the
separate no-follow helper, which rejects Unix final symlinks and Windows
reparse points. FIFO tests verify these reads return without waiting for a
writer.

### TNG-043 — Workflow run history and sample arrays grew without aggregate bounds

**Status: Resolved locally with persistence regressions** · **Priority: P2** · **Confidence: high**

Sidecar run history retains at most 200 records and 8 MiB of serialized JSON.
Each `matched`, `applied`, and `errors` array stores at most 32 samples while
the corresponding total fields preserve complete counts; error samples are
limited to 512 UTF-8 bytes. Oversized legacy history is not deserialized as an
unbounded value and the next diagnostic write recovers with bounded history.

### TNG-044 — Workflow output overflow waited for child pipe closure

**Status: Resolved locally with a process regression** · **Priority: P2** · **Confidence: high**

When stdout or stderr exceeds its configured cap, the workflow runner now
cancels sibling stream readers and the wait future, then kills and reaps the
child immediately. A regression uses a child that writes beyond the cap and
then sleeps, proving overflow does not wait for process timeout or pipe close.

### TNG-045 — qB RSS item state had no serialized-map ceiling

**Status: Resolved locally with SQLite regressions** · **Priority: P2** · **Confidence: high**

qB RSS item-map writes now reject serialized state above 8 MiB without
replacing the prior value. Legacy reads check SQLite byte length before
deserializing and reject values above 128 MiB; entry count and path/URL field
limits remain enforced at the API boundary.

### TNG-046 — Sidecar automation rule stores lacked aggregate JSON caps

**Status: Resolved locally with transactional regressions** · **Priority: P2** · **Confidence: high**

Workflow, ratio-group, native RSS, and qB RSS rule collections now enforce an
8 MiB serialized-state ceiling. Capacity errors map to `413`; failed writes
leave the previous SQLite value intact. Tests cover the shared vector writer
and ratio-group writer, with API/qB status mapping regressions.

### TNG-047 — OpenAPI and prose did not match the native and sidecar handlers

**Status: Resolved locally by source reconciliation and validation** · **Priority: P2** · **Confidence: high**

The native OpenAPI contract now reflects handler statuses, including JSON
`200` saved-view/RSS mutations and the queued `202` torrent-location update.
`docs/API.md` records arrangement-specific differences: native versus sidecar
add and tag responses, session-feature output, torrent-location jobs, and
request formats. OpenAPI validation passes at 59 paths / 80 operations.

### TNG-048 — Native WebUI torrent-add forms were rejected by the JSON-only route

**Status: Resolved locally with WebUI-shaped request regressions** · **Priority: P1** · **Confidence: high**

The shared WebUI sends multipart `FormData`; `torrentngd` previously extracted
only JSON, so native add requests failed before reaching the engine. The native
route now accepts its existing JSON format and bounded multipart forms, caps
multipart fields and torrent bytes, and rejects conflicting sources. Tests
cover WebUI magnet/file forms and ambiguous JSON/multipart sources.

### TNG-049 — Native rule upserts reported client errors as service outages

**Status: Resolved locally with status and state-preservation regressions** · **Priority: P2** · **Confidence: high**

Malformed rule entries and local capacity limits previously surfaced as
`503`. Native JSON-store upserts now reject invalid values with `400`, new
entries beyond the count cap with `429`, and aggregate serialized-state growth
with `413`, all before persistence. Regressions verify the status and that
rejected writes preserve prior state.

### TNG-050 — Native automation execution built unbounded selections and result arrays

**Status: Resolved locally with route and persistence regressions** · **Priority: P2** · **Confidence: high**

Native workflow and ratio-group matching previously collected the complete
registry selection and then built complete applied/error arrays, although the
API contract already set a 10,000-match ceiling and the run-history store had a
1 MiB budget. Matching now stops at 10,001 and returns `413` before action or
history mutation when the ceiling is exceeded. Direct-native responses and
history keep at most 32 `matched`/`applied`/`errors` samples with full totals;
error samples are truncated on UTF-8 boundaries to 512 bytes. History normalizes
legacy rows and evicts oldest records as needed to stay within both 200 rows and
1 MiB. The WebUI uses totals rather than sample lengths. Regressions cover the
10,001-match rejection, 100-match dry-run counts/samples, UTF-8 error samples,
and byte-budget eviction. This is functional API-boundary coverage, not torrent
capacity evidence.

### TNG-051 — Sidecar multipart limits made the documented maximum unreachable

**Status: Resolved locally with upload-path regressions** · **Priority: P2** · **Confidence: high**

The sidecar allowed 64 MiB torrent fields but capped the complete HTTP body at
64 MiB, leaving no room for multipart framing; Axum's default multipart body
limit also remained in force. The global transport ceiling is now 65 MiB, and
the Axum `DefaultBodyLimit` override is scoped only to the native-API and
qBittorrent torrent-add routes. Other JSON extractors keep the default smaller
limit. Local HTTP regressions upload an exact 64 MiB part through each add route
and verify a 3 MiB workflow JSON body still returns `413`. These exercise
documented request boundaries, not throughput or production capacity.

### TNG-052 — Native automation accepted oversized rule fields and RSS execution built incompatible results

**Status: Resolved locally with ingress, legacy-state, and RSS history regressions** · **Priority: P2** · **Confidence: high**

The native JSON-store aggregate cap still allowed one workflow/RSS string or
tag vector to be reused across many matches, and RSS sample request strings were
not enforcing the documented per-field limit. Native upserts now cap IDs, names,
and categories at 256 UTF-8 bytes; event/action tokens at 64; rule/filter/feed
text at 8,192; paths at 4,096; and tags at 1,024 values of at most 256 bytes
each. RSS titles and optional links are capped at 8,192 bytes, and apply rejects
blank titles. Previously stored oversized records remain listable, but matching
or execution fails closed. RSS apply now returns bounded string-name samples
with full totals and persists the same shape in workflow history. An apply with
no matching rules is a zero-action success and does not require a magnet link or
running client. Tests cover field/tag limits, HTTP request rejection, legacy
invalid state, empty apply titles, no-match behavior, and a 40-rule RSS
response/history.

### TNG-053 — Native set-category workflows ignored the documented destination field

**Status: Resolved locally with migration and selector regressions** · **Priority: P2** · **Confidence: high**

The native UI and API contract distinguish `category` (the match filter) from
`target_category` (the set-category destination), but native execution read the
filter field as the destination. A valid destination-only rule could therefore
clear a torrent's category, while a rule with both fields could write the
filter value. Native execution now prefers `target_category`; category-only
legacy rules migrate to the destination on read/upsert, and new or stored
set-category rules without a non-empty destination are rejected before actions.
Regression coverage verifies target precedence, source-category selection,
legacy migration, oversized stored rules, and missing-target rejection.

### TNG-054 — Native workflow run IDs collided within one-second timestamp resolution

**Status: Resolved locally with fixed-timestamp uniqueness regression** · **Priority: P2** · **Confidence: high**

Workflow and RSS history IDs were formed only from Unix seconds, so repeated
runs in the same second could persist duplicate IDs and collide in the WebUI's
`key={run.id}` list. Both run paths now append a UUID v4 while preserving
`started_at` as the display timestamp. A regression generates 100 IDs from the
same fixed timestamp and asserts uniqueness.

### TNG-055 — Sidecar storage capacity used the wrong statvfs byte multiplier

**Status: Resolved locally with portable arithmetic regression** · **Priority: P2** · **Confidence: high**

The sidecar multiplied `f_blocks` and `f_bavail` by `max(f_frsize, f_bsize)`.
POSIX block counts use `f_frsize` when nonzero; `f_bsize` is only the fallback
when `f_frsize` is zero. Choosing the larger field could overstate total and
available storage and distort UI capacity/usage percentages. The calculation
now uses the specified fragment size and the fallback is covered by a pure
test.

### TNG-056 — Recursive storage walks could exhaust the worker stack

**Status: Resolved locally with over-depth and directory-name regressions** · **Priority: P1** · **Confidence: high**

Storage-plan copy, delete, verification, and content-length walks recursively
descended directory trees without a depth guard. A sufficiently deep tree could
overflow a worker stack; a local regression also showed that a 256-level guard
did not fire before stack exhaustion. Both the descriptor-anchored Unix path
and portable planner now reject traversal beyond 64 directory levels. Failed
copies remove their partial destination, and rejected delete walks return an
error; callers must continue to treat an interrupted deletion as potentially
partial, as with other filesystem failures.

Unix directory verification also compared each destination entry against all
source entries, yielding quadratic work for wide directories. It now sorts the
two name lists and compares them in O(n log n), preserving order-independent
matching and detection of missing or extra entries. Regressions cover reordered
names, extra entries, and over-depth copy/delete/verification/content-length
walks in both implementations. Native non-Unix runtime and real-device behavior
remain unqualified.

### TNG-057 — Malformed bencode numeric tokens amplified memory use

**Status: Resolved locally with oversized-token regressions** · **Priority: P2** · **Confidence: high**

The bencode decoder limited nesting, node count, and string values, but scanned
integer tokens until their terminator and copied malformed numeric text into
error strings. A long integer or zero-prefixed byte-length token could therefore
cause avoidable input-proportional allocation while rejecting untrusted
metainfo. Integer scanning now stops beyond the 20-byte signed-`i64` width;
length-prefix scanning stops beyond a safe `usize` bit-width bound. Error
messages use short fixed reasons rather than echoing raw token contents.
Regressions exercise 1 MiB malformed integer and length-prefix inputs and
assert bounded error text. This is not a memory-profile or throughput claim.

### TNG-058 — Bencode integers accepted a non-canonical leading plus

**Status: Resolved locally with integer-form regressions** · **Priority: P2** · **Confidence: moderate**

Rust's `i64` parser accepts a leading `+`, so the decoder interpreted `i+1e`
and `i+01e` as valid integers despite already enforcing bencode canonicality
for negative zero and leading zeroes. The decoder now rejects any leading-plus
integer before numeric conversion. BEP 3 defines integers as base-10 tokens and
documents the negative form as `i-3e`, while explicitly rejecting leading-zero
encodings; treating `+` as non-canonical follows the decoder's strict grammar
and those examples, though the BEP text does not provide a separate formal
grammar production. See [BEP 3](https://www.bittorrent.org/beps/bep_0003.html).

### TNG-059 — BEP 52 duplicate file roots were rejected

**Status: Resolved locally with shared-layer and conflict regressions** · **Priority: P2** · **Confidence: high**

Both full v2 metainfo parsing and magnet piece-layer requirement extraction
treated a repeated pieces root as invalid. BEP 52 keys the top-level piece-layer
dictionary by pieces root and states that identical files always have the same
root, so identical large files can share one layer entry. The parser now keeps
one requirement per root when file length and layer hash count agree, allowing
one authenticated layer to serve both files and preventing duplicate network
fetches. If one root is paired with conflicting file requirements, parsing
still fails closed. Regressions cover a valid two-file shared layer through
full parsing and magnet planning, plus inconsistent metadata rejection. See
[BEP 52](https://www.bittorrent.org/beps/bep_0052.html).

### TNG-060 — Metainfo accepted paths storage plans could not traverse

**Status: Resolved locally with path-depth boundary regressions** · **Priority: P2** · **Confidence: high**

V1 metainfo parsing allowed up to 256 components in one path, while storage
plan copy/delete/verification walks now stop at 64 directory levels. That let
a torrent pass add-time parsing but fail later when a storage job traversed the
same deep tree. The metainfo path cap is now 64 components, so incompatible
paths fail at input validation. Tests prove a 64-component path is accepted
and a 65-component path is rejected. This is an intentional compatibility
limit until storage traversal becomes iterative or has a separately proven
stack bound.

## Claims to delete or downgrade now

Until the corresponding ledger item is resolved, these claims are not release
claims:

- “100k torrents” as a production capacity guarantee;
- “pure v2 metadata completion” without the bounded BEP 9/BEP 52 validation
  and evidence boundary documented in TNG-016;
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
| 2026-09-13 | Extended TNG-014's memory-accounting fix after a deeper engine pass: `PieceAssembly` allocations were bounded by a per-torrent 64 MiB soft ceiling but were not leased from the process-wide `PieceAssembly` governor class, leaving that allocation outside the total memory cap. Each live assembly now owns a governor lease, denied allocation is handled through the existing piece rejection path, and stats merge the live lease total with actor-reported bytes without double-counting. | Focused and full `rt-engine` tests pass after the change; format and diff checks pass. A peer-count/large-piece-count allocator profile remains deferred. | TNG-014's implementation scope is tightened; production-scale memory evidence remains open. |
| 2026-09-20 | Continued the protocol/egress audit: tracker, webseed, remote-torrent, and webhook HTTP clients now bypass ambient proxies and pin bounded, validated DNS results; IPv4/IPv6 special-use and transition ranges are filtered. Magnet parsing rejects conflicting repeated exact topics, hybrid magnets retain and verify both hashes across restart, and pure-v2 peer lookup uses an incremental collision-aware registry index. | Workspace tests and warnings-denied clippy pass; sidecar tests pass (165 unit / 87 integration); OpenAPI and certification self-tests pass. | Egress and hybrid-identity regressions are covered locally; public-network interoperability and external qualification remain separate evidence gates. |
| 2026-09-20 | Continued Windows and cross-platform storage hardening: runtime opens use reparse-point-aware handles and stable Win32 handle identity; the portable plan executor rejects reparse points, fails closed on ancestor-inspection errors, checks cancellation during copy/verification/delete, and reports partial deletion as uncertain. Fixed Windows absolute-drive path handling and same-volume move planning, plus FreeBSD `rlim_t` conversion and macOS test portability. | `cargo test -p rt-storage` (184 passed); Windows plan tests under Wine (54 passed); Windows full storage suite under Wine (151 passed / 2 Wine-limited identity failures); storage test targets compile for Windows, macOS, and FreeBSD; full workspace tests/clippy pass. | Static reparse-point and cancellation regressions are fixed. Windows plan-operation TOCTOU remains open; no native Windows runtime result is claimed. |
| 2026-09-20 | Follow-up bug/security pass: Windows storage-plan rename is atomic no-replace; parent creation and source/hash/destination file access reject reparse points; Windows import copies rather than invoking path-based hard-link creation. Rejected ambiguous v1/v2 truncated peer-wire identities, limited IPv6 egress to currently assigned IANA GUA prefixes, bounded SQLite hybrid-hash decoding before allocation, and expanded CI RustSec coverage to the root, sidecar, and fuzz lockfiles. Hardened local scan temp-file isolation. | Full workspace tests and strict clippy pass; sidecar tests (165 unit / 87 integration) and clippy pass; storage (185/185); Windows plan tests under Wine (56/56), full Windows suite under Wine (155/157; same two Wine-limited identity failures); macOS/FreeBSD storage test-target checks, OpenAPI, policy/bundle/storage self-tests, ShellCheck, formatting, and npm audit (0 vulnerabilities) pass. Local RustSec audit was unavailable because `cargo-audit` is not installed. | IANA's current registry still reserves unlisted space within `2000::/3` ([registry](https://www.iana.org/assignments/ipv6-unicast-address-assignments)). Windows recursive/path-based operation TOCTOU remains open, and Wine results are not native Windows qualification. |
| 2026-09-20 | Closed a recursive verification gap: the final destination-entry scan now checks cancellation and uses no-follow metadata on both source and destination; a deterministic regression swaps a source file for a symlink at that scan boundary. Windows recursive operations also retain no-delete-share ancestor/current-directory handles, and unit-only portable executor wrappers no longer emit production dead-code warnings. | Linux `rt-storage` tests pass (187/187); workspace tests and strict clippy pass; Windows plan tests under Wine pass (56/56), and the full Windows storage suite is 156/158 with the same two Wine delete-disposition failures. | The Windows executor remains path-based rather than fully handle-relative. Held directory handles narrow replacement races but do not constitute native Windows qualification or Unix `secure_fs` parity. |
| 2026-09-20 | Fixed hybrid metadata admission and BEP 47 padding semantics. The parser and engine share checks for payload order/path/length and v1-to-v2 piece alignment; rooted/rootless v2 layouts and valid optional tail padding are handled. Padding bytes are synthesized as zeroes for upload/verification, rejected if a peer supplies non-zero data, omitted from file projections, and not written to disk. `PieceMap` also rejects duplicate file IDs and padding IDs without spans. | Full workspace tests and warnings-denied clippy pass; sidecar tests (165 unit / 87 integration) and clippy pass; macOS/FreeBSD `rt-storage --tests` and Windows workspace test-target checks pass; Wine path/metainfo tests pass (17 / 61); npm audit reports zero vulnerabilities. The broad macOS engine cross-check needs a native Apple C toolchain for `ring`/SQLite and is not claimed. The workspace suite's synthetic scale regressions are not treated as capacity evidence; no dedicated capacity proof or soak was run. | Hybrid/parser/storage regressions are locally covered. Public-network compatibility and native Windows behavior remain unqualified; count and soak evidence are deferred to the later test phase. |
| 2026-09-20 | RustSec found sidecar `rustls 0.23.44` affected by RUSTSEC-2026-0285; updated its locked version to patched `0.23.45`. Also cleared the root lockfile's yanked `spin 0.9.8` warning by updating to `0.9.9`. | Sidecar tests (165 unit / 87 integration) and warnings-denied clippy pass. Current `cargo audit --file` passes for all three lockfiles without vulnerabilities or yanked warnings; root workspace tests and strict clippy pass after the spin update. The local container scan was not run because Trivy and the certification image are absent. | Rust dependency findings are resolved; container-image scanning remains locally unverified. |
| 2026-09-20 | Fixed sidecar path-authority gaps found during Windows target linting: session-metainfo and peer-ID reads now use a shared no-follow regular-file open; Windows opens and rejects the reparse object. Peer-ID repair now uses a unique same-directory temp file and replaces a symlink entry instead of writing through it. | Sidecar tests (167 unit / 87 integration) and Linux/Windows warnings-denied clippy pass; Unix target-preservation and Windows reparse-point target-preservation regressions pass under Wine. | Session `.torrent` and peer-ID suffix reads reject final-component symlink/reparse aliases. Windows identity remains Wine-tested, not natively qualified. |
| 2026-09-20 | Closed second-stage egress and peer-ban gaps before metainfo exists: BEP 9 `x.pe`, tracker/DHT-discovered, and forwarded metadata peers are filtered before retry-state admission and checked before TCP/uTP; the metadata actor shares global peer bans and cancels active work on eviction. The shared address policy also gates IPv4/IPv6 DHT destinations/results and full v1/hybrid/pure-v2 peer admission; private-network use remains opt-in. | `cargo test --locked -p rt-engine --lib` passes (427/427); `cargo clippy --locked -p rt-engine --all-targets -- -D warnings`, formatting, and `git diff --check` pass. Regression coverage verifies candidate filtering, ban eviction without reconnect, and IPv4/IPv6 DHT loopback opt-in. | Local paths are covered. No public-network test or soak was run in this pass; both remain deferred. |
| 2026-09-20 | Closed sidecar torrent API identity and resource-control gaps: native routes canonicalize cache hashes before backend access; bulk missing IDs are reported; native/qBittorrent selections, qBittorrent peer-address lists, and active fanout are bounded; literal dot hashes cannot normalize URL paths; FIFO state files are rejected without blocking. | Sidecar tests pass (175 unit / 94 integration); Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), and `git diff --check` pass. | Local API/filesystem regressions are covered; no public-network, capacity, or soak test was run. |
| 2026-09-20 | Closed TNG-031 qBittorrent compatibility resource/state gaps: delimited search/category/tag/tracker/file-priority and torrent-add URL inputs are incrementally bounded; plugin state and search-job history are capped; job selection is numeric; RSS items persist transactionally in SQLite with count/key bounds; RSS rules update by name and share a 1,024-rule cap across native and qBittorrent APIs. | Sidecar tests pass (175 unit / 94 integration); Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), and `git diff --check` pass. | RSS persistence and local resource bounds are covered by regressions; no public-network, torrent-capacity, or soak test was run. |
| 2026-09-20 | Closed TNG-032 metadata ingress bounds across root native and sidecar APIs: category/tag labels are limited to 256 UTF-8 bytes, category paths to 4,096 bytes, and per-mutation tag arrays to 1,024 items; root PATCH add/remove arrays share the cap and add-torrent/bulk label deserialization is bounded. Corrected the OpenAPI contract for bounded request bodies and the actual add/category/tag success statuses. | `rt-api-model` tests pass (20), `rt-api-native` tests pass (75), sidecar tests pass (175 unit / 94 integration); root and sidecar Linux warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), and `git diff --check` pass. | Local field/count rejection is regression-tested. Global category/tag cardinality, public-network behavior, capacity proof, and soak are not claimed. |
| 2026-09-20 | Closed the TNG-033 aggregate metadata growth gap: tag assignments are capped globally at 1,000,000 links / 64 MiB, SQLite summaries track writes/cascades and rebuild on startup, list/delta reads fail closed on over-limit legacy totals, qB multi-hash tag mutations preflight cumulative positive growth, torrent-add multipart fields are streamed under per-field byte caps, and native JSON tag arrays are bounded during deserialization. | Sidecar tests pass (183 unit / 96 integration); Linux and Windows GNU warnings-denied clippy and formatting pass. Regression coverage includes summary integrity/rebuild, exact count/byte math, legacy read rejection, multi-hash preflight, bounded JSON tag parsing, and oversized multipart text. | Local cache guards are covered. No public-network, torrent-count capacity proof, or soak test was run. |
| 2026-09-20 | Closed TNG-034 request amplification gaps: native JSON batch counts/string sizes and cross-seed operations are bounded; live-stats query entries/bytes are bounded before parsed-list materialization; qB `hashes=all` uses a 10,000-target SQL lookahead cap; nested tag/tracker/peer mutation products are capped before backend calls. | Sidecar tests pass (187 unit / 98 integration); Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), and `git diff --check` pass. Integration regressions prove oversized tracker/tag/peer fan-outs return `413` with zero backend calls. | Local guards are covered. Public-client interoperability, torrent-count proof, and soak remain deferred. |
| 2026-09-20 | Extended TNG-034 and closed TNG-035: ratio/workflow SQL selectors cap materialization at 10,001; oversized rule fan-out returns `413` before actions/history; ratio groups dispatch through the selected backend; workflow category filter/target fields are separated with legacy migration; RSS sample strings are bounded; stored ratio/workflow rule counts and field sizes are capped. | Sidecar tests pass (191 unit / 104 integration; 2 synthetic benchmark tests remain ignored); Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), `git diff --check`, WebUI build/lint, and WebUI tests (2) pass. | Rule boundaries and legacy behavior are locally regression-tested. No public-network, capacity proof, or soak test was run. |
| 2026-09-21 UTC | Closed deployment-auth and Phase 1 health gaps: optional qBittorrent/Transmission/Deluge stacks moved to separate overlays with required adapter credentials; backend WebUI/RPC host ports now bind to loopback; Transmission credentials are wired to the image's actual `USER`/`PASS` variables. The unauthenticated Phase 1 ruTorrent UI now binds to loopback by default. Fixed Phase 1 Nginx's PHP-FPM upstream, replaced the nonexistent `/index.php` certification probe with ruTorrent's JSON settings endpoint, and made the healthcheck exercise PHP-FPM. Local certification/interop management, tracker, and fixture ports plus native Prometheus/Grafana host ports now bind to loopback. Removed the interop runner's static `adminadmin` login attempt; it uses qBittorrent's temporary startup password or an explicitly supplied lab credential. | Default, three backend overlays, certification, Phase 1, interop, and native Compose contracts validate; default Compose needs no optional-backend credentials, while each selected overlay rejects missing credentials. Full disposable Phase 1 certification passes (HTTP, PHP-FPM, versions, SCGI, processes, TCP/UDP); Compose-equivalent healthcheck reaches `healthy`. Full Rust workspace and sidecar tests pass (191 unit / 104 integration); warnings-denied clippy, formatting, WebUI build/lint/tests (2), npm audit, all three RustSec audits, image CVE scan, and deployment-hardening contracts pass. Trivy config still fails only on two HIGH `DS-0002` root-user findings. `bash -n`, ShellCheck, and `git diff --check` pass. | Profile credential and unauthenticated UI exposure defects are fixed locally. Non-root migration for existing data/LVM ownership remains open; no production deployment, public-torrent, torrent-count, or soak test was performed. |
| 2026-09-21 UTC | Closed both `DS-0002` root-container findings: compatible-client and Phase 1 containers now reject UID 0, use configurable nonzero UID/GID with matching volume ownership, and mount operator config read-only; Phase 1 uses internal port 8080 and caps nginx at two workers. Entrypoints write user overrides under `/run` and use `tini`; missing sidecar config falls back to the packaged read-only default. | Rootless Phase 1 certification passes HTTP/PHP-FPM, rTorrent/SCGI, process, TCP/UDP, runtime-UID, and bounded-worker checks. Sidecar runtime returns health 200, authenticated engine API 200 / unauthenticated 401, connects to SCGI, and writes fresh data/session/state volumes as UID 1000; custom `12345:23456` volume ownership and root-entrypoint rejection also pass. `scripts/security_scan.sh` passes npm, all three RustSec lockfiles, Compose hardening, Trivy image, and Trivy config checks. | Tracked-volume ownership migration and host/LVM mount permissions remain unverified and must be applied only to the selected deployment. No production data, LVM target, public torrent, or soak was used. |
| 2026-09-21 UTC | Fixed the Phase 1 incoming-port override: `PHASE1_INCOMING_PORT` now controls host publication while `PHASE1_CONTAINER_INCOMING_PORT` controls rTorrent's internal TCP/UDP listener; certification discovers the runtime internal port before inspecting Docker mappings. Exercised the documented root-owned-volume migration on disposable data/session/state volumes and confirmed contents survive while post-migration rootless writes succeed. | [Phase 1 certification](../certification/reports/phase1-cert-rootless-custom-ports-20260921.md) passes with host port 51001 mapped to container port 52000 on TCP and UDP, UID 1000, two nginx workers, PHP-FPM, and SCGI. [Compatible-client smoke](../certification/reports/rootless-compatible-client-smoke-20260921.md) records API/auth, volume, custom-UID, root-refusal, and migration checks. [Security scan](../certification/reports/security-scan-rootless-20260921.md) passes npm, main/sidecar/fuzz RustSec, Trivy image, and Trivy config checks. | Existing production volume ownership and host/LVM mount permissions remain deployment-specific and unverified. No production data, LVM target, public torrent, or soak was used. |
| 2026-09-21 UTC | Closed two panic-amplification edges: storage-job recovery now returns a poisoned-DB error rather than panicking while containing a worker failure, and session snapshot/shard caches recover poison so list reads can rebuild their derived projections. Fixed ShellCheck findings in the security-scan contract and Phase 1 socket wait. | Full locked workspace tests and warnings-denied clippy pass; sidecar tests (191 unit / 104 integration), clippy/formatting, WebUI tests/lint/build, ShellCheck, shell syntax, and the refreshed [security scan](../certification/reports/security-scan-rootless-20260921.md) pass. The scan includes custom host 51001 → container 52000 TCP/UDP validation and reports no HIGH/CRITICAL image/config findings. | These are local failure-containment fixes. No public torrent, capacity proof, production volume/LVM, or soak was used. |
| 2026-09-21 UTC | Extended panic/error-boundary and script audit: poisoned SQLite handles return a persistence error; poisoned derived session snapshot caches recover for rebuild; ShellCheck now passes warning severity across all `.sh` files after fixing an ambiguous security-review key expansion, ignored Compose override declarations, VPN child-environment construction, ROOT-prefix quoting, and socket-wait bookkeeping. Native scale report now includes the already-measured post-promotion/restart metrics latencies. | Full locked workspace tests/clippy; sidecar (191 unit / 104 integration), clippy/format; WebUI tests/lint/build; all-script `shellcheck -S warning`; Bash syntax on all `.sh`, POSIX syntax on container entrypoints; security-review smoke with explicit disposable credentials; and refreshed [security scan](../certification/reports/security-scan-rootless-20260921.md) pass. | Repository-local checks pass at warning severity; informational/style ShellCheck notices are not promoted to errors. No public torrent, capacity proof, production volume/LVM, or soak was used. |
| 2026-09-21 UTC | Closed TNG-036 panic-amplification paths across native/qB sort caches, egress/storage caches, disk queues, admission/rate state, idempotency claims, prepared-file bookkeeping, and engine shutdown task handles. Disposable caches rebuild; authoritative state is preserved. | `cargo test --workspace --all-targets --locked --quiet` passes; targeted suites pass (21 model, 76 native API, 90 qB API, 436 engine, 194 storage); workspace warnings-denied clippy, formatting, and `git diff --check` pass. | TNG-036 resolved locally with panic-injection coverage. No public torrent, capacity proof, production volume/LVM, or soak was run. |
| 2026-09-21 UTC | Closed TNG-050: native workflow and ratio-group selection now returns `413` above 10,000 matches before actions, immediate results and history use 32-item samples with full totals and UTF-8-safe 512-byte errors, and native history evicts oldest runs to honor its 1 MiB cap. Updated OpenAPI and WebUI to reflect result totals. | Full locked workspace tests and warnings-denied clippy pass; native API tests (88); WebUI tests (3 across 2 files), lint, and production build pass; formatting, OpenAPI validation (59 paths / 80 operations), JSON parse, and `git diff --check` pass. | Local automation-boundary regressions are covered; no public torrent, capacity proof, production volume/LVM, or soak was run. |
| 2026-09-21 UTC | Closed TNG-051: raised the sidecar request ceiling to 65 MiB for 64 MiB torrent fields plus multipart envelope; scoped Axum's raised body limit to native/qB torrent-add routes so JSON endpoints retain the default. | Sidecar tests pass (206 unit / 110 integration); Linux and Windows GNU warnings-denied clippy, formatting, and `git diff --check` pass. Exact 64 MiB uploads pass through both add routes; a 3 MiB workflow JSON body still returns `413`. | Upload boundary behavior is fixed locally; no public torrent, capacity proof, production volume/LVM, or soak was run. |
| 2026-09-21 UTC | Closed TNG-052 through TNG-054: bounded native rule/request fields and RSS outputs/history; made RSS no-match apply a zero-action success; separated workflow category filters from destinations with legacy migration; and made workflow/RSS run IDs unique within the same second. Refreshed OpenAPI and WebUI count labels. | Full locked workspace tests and warnings-denied clippy pass; native API tests pass (97), including `cargo +1.88.0 test -p rt-api-native --locked --lib`; WebUI tests (3 across 2 files), lint, and build pass; root formatting, OpenAPI validation (59 paths / 80 operations), JSON parse, and `git diff --check` pass. | API, migration, and identifier regressions are covered. No public torrent, capacity proof, production volume/LVM, or soak was run. |
| 2026-09-21 UTC | Closed TNG-055: sidecar storage-capacity math now uses `f_frsize` for statvfs block counts and falls back to `f_bsize` only when needed. | Sidecar tests pass (207 unit / 110 integration); Linux and Windows GNU warnings-denied clippy, formatting, OpenAPI validation (59 paths / 80 operations), JSON parse, and `git diff --check` pass. | Arithmetic is regression-tested without claiming real-device storage qualification. No public torrent, capacity proof, production volume/LVM, or soak was run. |
| 2026-09-21 UTC | Closed TNG-056: recursive storage-plan copy/delete/verify/content-length walks now reject nesting beyond 64 levels; Unix directory verification compares sorted entry names rather than using a quadratic nested scan. | Full locked workspace tests pass, including 197 storage tests; warnings-denied workspace clippy and formatting pass. Over-depth and unordered/extra-entry regressions cover both storage implementations. | Stack-exhaustion and wide-directory verification defects are fixed locally. Non-Unix runtime and real-device behavior remain unqualified; no torrent-count proof or soak was run. |
| 2026-09-21 UTC | Closed TNG-057: bencode integer tokens and byte-length prefixes are rejected during bounded scanning, and malformed-number diagnostics no longer copy untrusted token contents. | `cargo test -p rt-bencode --locked` passes (15 tests), including 1 MiB integer and length-prefix regressions; full workspace tests, warnings-denied clippy, formatting, and `git diff --check` are rerun for handoff. | Malformed metainfo rejection avoids token-sized error allocations; this is not a parser allocation/throughput benchmark. No torrent-count proof or soak was run. |
| 2026-09-21 UTC | Closed TNG-058: reject bencode integers with a leading `+`, including `i+1e` and `i+01e`, instead of inheriting Rust integer parser syntax. | Bencode tests cover both forms; full workspace tests, warnings-denied clippy, formatting, and `git diff --check` pass. | Strict canonical interpretation is regression-tested locally; confidence is moderate because BEP 3 illustrates and constrains integer forms without spelling out a formal grammar production for `+`. No torrent-count proof or soak was run. |
| 2026-09-21 UTC | Closed TNG-059: duplicate large v2 files may reuse one pieces-root-keyed piece layer; magnet requirements fetch that layer once, while mismatched requirements for one root are rejected. | `cargo test -p rt-metainfo --locked` passes (62 tests), including full-metainfo sharing, magnet planning deduplication, and inconsistent-root rejection; final full-workspace validation follows. | BEP 52 identical-file interoperability gap is closed locally. No public-network or capacity claim is implied. |
| 2026-09-21 UTC | Closed TNG-060: v1 torrent paths are bounded to 64 components, matching the storage-plan recursion guard so deep paths are rejected during metainfo validation. | `cargo test -p rt-metainfo --locked` passes (63 tests), including paths at the 64-component acceptance boundary and 65-component rejection boundary; final full-workspace validation follows. | Later move/delete/verification failures for untraversable metainfo paths are prevented at add time. This is an intentional compatibility limit. |
| 2026-09-21 UTC | Closed TNG-061: malformed private flags now fail parsing instead of silently enabling public-torrent behavior; only absent/0 and 1 are accepted. | Full locked workspace tests, warnings-denied clippy, formatting, and `git diff --check` pass; metainfo tests include absent/0/1, wrong-type, and unknown-integer cases. | Invalid metadata cannot reach DHT/PEX admission as public. BEP 27 private-tracker lifecycle is tracked separately in TNG-062. |
| 2026-09-21 UTC | Closed TNG-062: private v1/hybrid and pure-v2 torrents announce to one tracker, fail over in configured order after failure, and drop old-tracker peers/allowlists; stopped announces target only the active tracker. | Full locked workspace tests pass, including 440 `rt-engine` tests; warnings-denied clippy, formatting, and `git diff --check` pass. Focused regressions cover v1 and pure-v2 peer/allowlist cleanup. | Private tracker isolation now follows BEP 27 locally; no external tracker interoperability claim is implied. |
| 2026-09-21 UTC | Closed TNG-063: tracker log labels redact userinfo/path/query/fragment and reqwest send/body errors omit the request URL. | Full locked workspace tests, warnings-denied clippy, formatting, and `git diff --check` pass; focused HTTP/UDP URL-redaction tests pass. | Direct passkey-bearing URL logging paths are closed. Arbitrary tracker-supplied failure text remains untrusted. |
| 2026-09-21 UTC | Opened TNG-064 after reviewing parser and admission paths: the fixed metadata lease does not cover the bencode node tree and undercounts hybrid raw copies. | Source inspection confirms the `2 * raw_len + 64 KiB` estimate, 1,000,000-node decoder bound, and two owned raw buffers in hybrid metadata. | A fixed multiplier change would not bound decoder allocations; allocation-aware accounting or a justified explicit limit is still required. |
| 2026-09-21 UTC | Closed TNG-064 through TNG-070 locally: parser admission covers collection reallocation overlap, structured metainfo projections and raw copies; curl policy blocks transfer resets, credential-routing redirects, query/URL/resolver and scheme-less multi-target bypasses while preserving URL case; rTorrent and native JSON base64 uploads reject oversized envelopes before decoding; webseed de-duplication is no longer quadratic. | Focused locked Rust tests pass (`rt-bencode` 18, `rt-metainfo` 70, `rt-engine` 441, `rt-api-rtorrent` 27 unit / 2 integration, `rt-api-native` 98); workflow/protected-target/API-load Python tests pass (33); Bash syntax, warning-level ShellCheck, formatting, and `git diff --check` pass. | Engine-admitted parser bounds and local script/API regressions are covered. Standalone parser callers, allocator/RSS behavior, hosted workflows, public-network behavior and soak remain outside this verification. |
| 2026-09-21 UTC | Closed TNG-071 through TNG-080 locally: Deluge selects one Base64 engine; curl rejects URL glob re-enablement, credential-bearing GET-data, unsafe cookie/netrc files, libcurl source export, response-header write-out, and unspooled inline JSON; Referer values are privately spooled and treated as credentials. | Deluge tests pass (30), native API tests pass (98), and all 33 workflow/protected-target/API-load Python tests pass. Bash syntax and warning-level ShellCheck pass across repository scripts/deploy; `cargo fmt --all -- --check` and `git diff --check` pass. Full locked workspace tests and warnings-denied clippy had passed after the Rust changes in TNG-071; no Rust code changed in TNG-072–080. | Local policy and payload regressions are covered. Curl write-out is intentionally limited to the status and elapsed-time fields used by the repository. Hosted CI, public-network behavior, torrent-count proof, and soak remain unclaimed/deferred. |
| 2026-09-21 UTC | Closed TNG-081: the default metainfo convenience APIs now use a cumulative 512 MiB allocation cap and enforce the 64 MiB input boundary; callers needing process-wide admission can supply their own reservation callback. | `cargo test -p rt-metainfo --locked` passes (71); `cargo test --workspace --all-targets --locked --quiet` and warnings-denied workspace clippy pass; formatting and `git diff --check` pass. | Per-call parser allocations are bounded locally. The cap is not an RSS guarantee and does not bound aggregate concurrent calls; no torrent-count proof, public-network test, or soak was run. |
| 2026-09-21 UTC | Closed TNG-082: authenticated curl GET policy now recognizes both `@file` and `name@file` `--data-urlencode` inputs as opaque query data, in both separated and equals option forms. | All 35 Python workflow/protected-target/API-load tests pass; Bash syntax, warning-level ShellCheck across `scripts/` and `deploy/`, and `git diff --check` pass. | No curl process is launched for uninspectable authenticated GET query data. No public-network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-083: the protected-target cleartext exception is now limited to RFC 1918 IPv4 and IPv6 ULA instead of Python's broader `is_private` classification of non-globally-reachable special ranges. | All 35 Python workflow/protected-target/API-load tests pass; Bash syntax, warning-level ShellCheck across `scripts/` and `deploy/`, and `git diff --check` pass. | Explicit private HTTP targets now match the policy name; no network request was made. No torrent-count proof or soak was run. |
| 2026-09-21 UTC | Closed TNG-084: the bundled Nginx WebUI, API, and WebSocket locations now drop client-supplied `X-Remote-User`; configuration and security docs distinguish the bundled default from custom trusted proxies. | The workflow-security regression checks every proxied location; the combined Python suites pass (36 tests). Bash syntax, warning-level ShellCheck, and `git diff --check` pass. | Header-based auth remains disabled in the shipped sidecar config; custom auth proxies still own caller authentication. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-085: base and optional compatible-client APIs, the HTTP-only Nginx listener, and the native HTTP API now bind to `127.0.0.1` in Compose by default; peer ports remain published. | The 37 Python workflow/target/API-load tests pass; rendered sidecar and native Compose configs confirm loopback management bindings. Bash syntax and `git diff --check` pass. | Remote management requires an authenticated TLS front end or an explicit protected override. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-086: signed sidecar session cookies now default to `Secure`; logout emits matching secure deletion cookies, and disabling the flag is rejected for non-loopback binds. | Sidecar tests pass (208 unit / 112 integration); Linux warnings-denied clippy, formatting, and `git diff --check` pass. Regressions cover legacy config defaults, explicit local opt-out, login/logout headers, and public-bind validation. | Cookie confidentiality is strengthened when access uses TLS. Plain-HTTP remote access remains unsupported; no network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-087: sidecar HTTP transport, status, and bounded body-read errors no longer retain request URLs that may carry query credentials. | Sidecar tests pass (210 unit / 112 integration); Linux warnings-denied clippy, formatting, and `git diff --check` pass. Focused refused-connection and HTTP-500 regressions verify query secrets are absent from the complete error chain. | No external request was made; Windows GNU sidecar checks were not rerun. Torrent-count proof and soak remain deferred. |
| 2026-09-21 UTC | Closed TNG-088: sidecar tracker URL diagnostics now retain only origins; tracker mutation API responses and logs omit raw URLs and backend error text that could echo passkeys. | Sidecar tests pass (213 unit / 112 integration); warnings-denied clippy, formatting, and `git diff --check` pass. Regression tests cover userinfo/path/query/fragment redaction and secret-free API failure summaries; 37 Python workflow/protected-target/API-load tests remain green. | No public-network or tracker request was made. Windows GNU sidecar checks, torrent-count proof, and soak remain deferred. |
| 2026-09-21 UTC | Closed TNG-089: magnet, URL, metainfo, and RSS ingestion failure diagnostics no longer log or return arbitrary backend error text that could echo passkey-bearing inputs. | Sidecar tests pass (214 unit / 112 integration); warnings-denied clippy, formatting, and `git diff --check` pass. The RSS failure-summary regression and URL/magnet source-redaction tests pass. | Backend error content is no longer included in these diagnostics; whether a particular backend echoes inputs was not tested. No external client/network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-090: workflow webhook and RSS feed URL list rows now display only a validated HTTP(S) origin, hiding URL credentials and endpoint details. | All 7 WebUI tests pass; TypeScript, ESLint, and an isolated production build pass. Helper tests cover malformed/non-HTTP(S) input; component regressions assert secrets are absent from both rendered panels. | This masks list labels; configuration inputs remain deliberately visible while editing. No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-091: tracker URL labels now retain only a validated HTTP(S)/UDP origin, removing heuristic leaks through unknown query names, short path tokens, fragments, and userinfo. | All 7 WebUI tests pass; TypeScript, ESLint, isolated production build, and `git diff --check` pass. Regressions cover the previous bypass shapes plus UDP and malformed/unsupported inputs. | Full tracker URLs remain available only through the explicit detail reveal/copy controls. No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-092: rTorrent log lines now redact scheme-qualified URLs and embedded magnets before persistence/API exposure, and mask common standalone credential-like query keys. | Sidecar tests pass (215 unit / 112 integration); Linux warnings-denied Clippy, formatting, and `git diff --check` pass. Regressions cover userinfo, short path passkeys, unknown query names, fragments, FTP/UDP, magnets, and standalone signature/auth-key values. | Only scheme/host/port context is retained for URLs; arbitrary non-URL secrets in backend log text are not comprehensively classified. No external network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-093: tracker-supplied warning and failure text now redacts echoed URLs, magnets, and common credential-like query fields before logs, state retention, and API projection; terminal controls are removed. | `rt-tracker` tests pass (88); full locked workspace tests, warnings-denied Clippy, formatting, and `git diff --check` pass. Parser-level and direct-state regressions cover echoed URL credentials, encoded key names, terminal controls, and ordinary-text preservation. | Tracker messages remain operator-visible after sanitization; unrelated arbitrary secret formats are not exhaustively classifiable. No external tracker request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-094: workflow script stdout/stderr now escape record-breaking, terminal, Unicode line-separator, and bidi controls before entering logs. | Sidecar tests pass (216 unit / 112 integration); Linux warnings-denied Clippy, formatting, and `git diff --check` pass. Regression covers C0/C1 controls, newline injection, ESC, Unicode line separators, bidi overrides/isolates, and preserved ordinary Unicode. | This prevents control-based log/terminal spoofing; it does not classify arbitrary secrets printed by an explicitly configured script. No network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-095: move and staged-copy import plans now fail closed at preview on targets without an atomic no-replace rename primitive, rather than claiming they can apply and failing after job submission. | Storage tests pass (199) and native API tests pass (99); storage/API test targets compile for FreeBSD and Windows GNU, and storage compiles for macOS. The combined macOS API cross-check is blocked by unavailable Apple-target C compilation for dependencies. | Direct hard-link imports remain applicable; no racy overwrite fallback was introduced. Cross-compilation is not native runtime qualification. No network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-096: persisted rTorrent log messages now escape terminal, Unicode line-separator, and bidi controls after URL/secret redaction and before event storage. | Sidecar tests pass (218 unit / 112 integration); Linux warnings-denied Clippy, formatting, and `git diff --check` pass. The sidecar test target cross-compiles for Windows GNU. Persisted-event regressions cover C0/C1, ESC, bidi controls, and readable Unicode. | URL/magnet/common credential redaction remains bounded to recognized forms; arbitrary non-URL secret formats are not exhaustively classified. No network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-097: qBittorrent, Transmission, rTorrent, and remote TorrentNG tracker/status text now redact echoed tracker credentials before cache/API projection. | Sidecar tests pass (223 unit / 112 integration); Linux warnings-denied Clippy, formatting, and `git diff --check` pass. Adapter regressions cover embedded URLs, path/query secrets, encoded standalone keys, controls, and ordinary text; the sidecar test target cross-compiles for Windows GNU. | Recognized URL and credential-field formats are sanitized; arbitrary non-URL secrets are not exhaustively classifiable. No network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-098: native compatibility tracker snapshots and sidecar torrent-cache reads now re-sanitize legacy warning/failure/status strings loaded from SQLite. | `rt-tracker` tests pass (89); sidecar tests pass (224 unit / 112 integration); the engine persisted-row regression, warnings-denied Clippy, formatting, `git diff --check`, and Windows GNU sidecar test-target cross-check pass. Full locked workspace tests and warnings-denied workspace Clippy pass. | New writes were already protected; this closes old-row read paths without rewriting either database. Arbitrary non-URL secrets remain outside recognized forms. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-099: operator-event message and JSON payload redaction now applies on write and when projecting legacy `app_events` rows to native and qBittorrent logs. | Sidecar tests pass (228 unit / 112 integration); warnings-denied Clippy, formatting, `git diff --check`, and Windows GNU sidecar test-target check pass. Regressions inspect raw stored values and seed a legacy SQLite row with URL/query credentials, file paths, split-line secrets, and controls. | JSON shape and ordinary text are retained; invalid legacy payload JSON projects as `{}`. Basenames and arbitrary unclassified secrets may remain. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-100: dynamic error fields across sidecar API, qBittorrent, startup, stats, and sync tracing now use shared redaction; rTorrent poll/recovery fields are sanitized too. | Sidecar tests pass (229 unit / 112 integration); the error-chain redaction regression, warnings-denied Clippy, formatting, `git diff --check`, and Windows GNU sidecar test-target check pass. A source scan confirms no direct `error = %e`/`error = %error` fields remain. | Non-error dynamic tracing fields remain separate audit surfaces. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-101: user-controlled structured log labels, request path/method values, backend torrent identifiers, and configured data paths now use shared display redaction before emission. | Sidecar tests pass (230 unit / 112 integration); structured-field regression covers newline, ESC, bidi, URL userinfo/query credentials, and a local path; warnings-denied Clippy, formatting, `git diff --check`, and Windows GNU sidecar test-target check pass. | This sanitizes recognized URL/path/credential patterns and controls; arbitrary unclassified non-URL secrets remain outside the guarantee. No public-network test, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-102: native webseed URL fields now retain only origins, reqwest errors discard their URLs, and warning text is sanitized before tracing. | `rt-engine` tests pass (443); locked offline workspace tests, warnings-denied workspace Clippy, formatting, and `git diff --check` pass. Regressions cover origin-only URL labels and webseed error credentials/controls. | Only local/unit regressions were run; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-103: qBittorrent URL-fetch transport errors no longer retain reqwest URLs, and source labels drop userinfo, path, query, and fragment. | `rt-api-qbit` tests pass (91); locked offline workspace tests, warnings-denied workspace Clippy, formatting, and `git diff --check` pass. A loopback connection-reset regression verifies secrets are absent. | The failure-path regression uses only a local listener; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-104: explicit Tokio task-join errors no longer format panic payloads into logs, compatibility API failures, torrent error state, or durable storage-job reasons; fastresume and daemon CLI worker boundaries use static summaries too. | Locked offline workspace tests pass, including 444 `rt-engine` tests; `rt-fastresume` tests pass (29); warnings-denied workspace Clippy, formatting, and `git diff --check` pass. Engine and fastresume panic-canary regressions pass. | The process panic hook remains a separate stderr surface and was not changed. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-105: `torrentngd` now installs a panic hook before argument/config handling that writes source location and a fixed omission message without printing payload text. | An isolated subprocess regression verifies `main.rs` location and the fixed message appear while a panic canary is absent; full locked offline workspace tests, warnings-denied Clippy, formatting, and `git diff --check` pass. | This applies to `torrentngd`; applications embedding the engine library retain responsibility for their process panic hook. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-106: DbWorker shutdown now classifies native-thread and Tokio wrapper join failures, marks the worker unhealthy, and recovers a poisoned handle lock. | Full locked offline workspace tests pass (451 `rt-engine` tests); focused uTP tests pass (43); warnings-denied workspace Clippy, formatting, and `git diff --check` pass. Regressions cover inner thread panic, wrapper panic/cancellation/success, unhealthy state, and lock poisoning. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-107: bounded shutdown helpers now observe completed JoinErrors and abort-grace expiry across engine, DHT, peer, storage-job, session-event, v2 peer, and uTP receive-task lifecycles. | Full locked offline workspace tests pass (451 `rt-engine`, 43 `rt-utp`); warnings-denied workspace Clippy, formatting, and `git diff --check` pass. Shared-helper timeout/panic/success and uTP panic/poisoned-lock regressions pass. | Cancellation after an explicit abort is expected; panic outcomes remain visible without payloads. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-108: peer upload-read diagnostics now summarize Tokio task-join failures rather than formatting JoinError. | Full locked offline workspace tests pass (451 `rt-engine`); the panic-canary summary regression, warnings-denied workspace Clippy, formatting, and `git diff --check` pass. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-109: sidecar cache writer mutex poisoning now fails closed, while read-pool checkout skips poisoned slots and errors cleanly if all are poisoned. | Sidecar locked offline tests pass (235 library / 3 binary / 112 compatibility); writer/single-reader/full-pool poison regressions, warnings-denied Clippy, formatting, and `git diff --check` pass. | The potentially interrupted SQLite connection is not recovered or reused. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-110: sidecar `spawn_blocking` failures now use payload-free task/outcome summaries, and cache errors do not retain Tokio JoinError sources. | Sidecar locked offline tests pass (235 library / 3 binary / 112 compatibility); panic-canary error-chain regression, warnings-denied Clippy, formatting, and `git diff --check` pass. | Tracker lookup worker failure now warns before using the empty fallback. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-111: sidecar now supervises sync, stats, and optional rTorrent log-ingestion loops and terminates on an unexpected task exit instead of silently keeping HTTP alive. | Sidecar locked offline tests pass (235 library / 3 binary / 112 compatibility); panic/cancellation/normal-return regressions, warnings-denied Clippy, formatting, and `git diff --check` pass. | Broken loops fail-stop; restart is delegated to the process supervisor. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-112: rTorrent low-priority RPC timeout handling now awaits its 15-second circuit-breaker update before returning. | Sidecar locked offline tests pass (235 library / 3 binary / 112 compatibility); the stalled Unix-SCGI regression confirms the immediate next background RPC is rejected; warnings-denied Clippy, formatting, and `git diff --check` pass. | Local Unix-socket test only; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-113: sidecar live-speed writes now use an exclusive UUID temp file and atomic rename rather than following the predictable `.tmp` path. | Sidecar locked offline tests pass (235 library / 3 binary / 112 compatibility), including the planted-symlink regression; warnings-denied Clippy, formatting, and `git diff --check` pass. | The configured parent directory must remain service-owned; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-114: the default rTorrent UI overlay now uses persistent writable state instead of the read-only `/config` bind; the UI probes actual writability and new Unix overlay files are owner-only. | Sidecar locked offline tests pass (238 library / 3 binary / 112 compatibility); all 38 deployment Python tests, warnings-denied Clippy, formatting, shell syntax, and `git diff --check` pass. | The entrypoint imports only operator-configured, validated paths; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-115: sidecar diagnostic redaction and authenticated curl policy now repeatedly decode up to eight percent-encoding layers before classifying credential-like query keys. | Sidecar locked offline tests pass (239 library / 3 binary / 112 compatibility); all 38 deployment Python tests, warnings-denied Clippy, formatting, shell syntax, and `git diff --check` pass. | Redaction remains scoped to recognized credential names; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-116: the curl wrapper and sidecar redactor now classify pass/password aliases, torrent/tracker pass, passphrase, compound secret-key, bearer/credential, and private-tracker pid/uk/sig fields consistently. | Sidecar locked offline tests pass (240 library / 3 binary / 112 compatibility); all 38 deployment Python tests, warnings-denied Clippy, formatting, shell syntax, and `git diff --check` pass. | Recognition remains deliberately scoped to credential-like names; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-117: sidecar text and rTorrent-log redaction now recognizes common Authorization, API-key, cookie, and other header-style credential values. | Sidecar locked offline tests pass (242 library / 3 binary / 112 compatibility); multi-token header and following-line regressions, all 38 deployment Python tests, warnings-denied Clippy, formatting, shell syntax, and `git diff --check` pass. | Remaining redaction is bounded to recognized header/credential forms; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-118: query-value redaction no longer short-circuits path masking; sensitive query tokens hide their full path and drop fragments. | Sidecar locked offline tests pass (242 library / 3 binary / 112 compatibility); path/query/fragment regressions, all 38 deployment Python tests, warnings-denied Clippy, formatting, shell syntax, and `git diff --check` pass. | The change concerns diagnostic redaction only; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-119: curl header-file classification rejects FIFOs, symlinks, and other non-regular sources before any blocking read or curl invocation. | All 39 deployment Python tests pass, including FIFO timeout, symlink rejection, and fake-curl non-invocation; Bash syntax and `git diff --check` pass. | No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-120: the curl wrapper now strips all non-ASCII-alphanumeric punctuation from query keys, matching the sidecar credential matcher. | All 39 deployment Python tests pass, including punctuation-obfuscated URL/GET credential names; Bash syntax and `git diff --check` pass. | No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-121: native tracker-warning/failure redaction now covers sidecar credential aliases, header-style values, and path/query/fragment combinations. | `rt-tracker` tests pass (90); warnings-denied crate Clippy, workspace formatting, and `git diff --check` pass. | No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-122: sidecar and native tracker redactors now drop query fragments after replacing a sensitive value, while preserving safe query fields. | Full locked offline workspace and sidecar tests pass (`rt-tracker`: 90; sidecar: 242 library / 3 binary / 112 compatibility); warnings-denied workspace/sidecar Clippy, all 39 deployment Python tests, shell syntax, formatting, and `git diff --check` pass. | No network, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-123: sidecar and native tracker redactors classify credential keys and header names through escaped control/format characters and multi-line seams without changing established Sidecar escaped-log output; ambiguous long chains are bounded and fail closed. | Full locked offline workspace suite passed; final `rt-tracker` tests (90) and Sidecar tests (242 library / 3 binary / 112 compatibility) pass after the line/header refinements; warnings-denied `rt-tracker` and Sidecar Clippy, all 39 deployment Python tests, formatting, shell syntax, and `git diff --check` pass. Existing local scale unit tests ran as part of all-targets, but are not capacity qualification evidence. | No public-network test or soak was run. |
| 2026-09-21 UTC | Closed TNG-124: remote webhook and user-supplied torrent egress now reject every non-2xx status, including un-followed redirects, instead of treating a 3xx response as success. | The locked offline Sidecar suite passes (248 library / 3 binary / 114 compatibility; two synthetic benchmarks remain ignored); the loopback webhook 302 regression confirms rejection, no request to the redirect target, and no query-secret leakage. Warnings-denied Clippy, formatting, and `git diff --check` pass. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-125: qBittorrent, Deluge, Transmission, and TorrentNG adapter clients now disable redirects and fail closed on 3xx responses, including direct TorrentNG mutations. | Four adapter-specific loopback 302 regressions verify each call errors and no request reaches the redirect target; the locked offline Sidecar suite passes (248 library / 3 binary / 114 compatibility), warnings-denied Clippy, formatting, and `git diff --check`. | No external backend, public-network request, torrent-count proof, or soak was used. |
| 2026-09-21 UTC | Closed TNG-126: unauthenticated browser-marked login/logout requests and trusted-proxy-authenticated mutations/WebSockets now enforce same-origin checks; bearer API tokens retain their non-ambient path. | The locked offline Sidecar suite passes (248 library / 3 binary / 114 compatibility), including cross-origin login rejection with no session cookies, proxy mutation/WebSocket rejection, same-origin acceptance, and bearer compatibility; warnings-denied Clippy, root/Sidecar formatting, and `git diff --check` pass. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-127: stale unsigned `tng_session` cookies no longer shadow a later valid `SID` in loopback-only legacy mode. | The locked offline Sidecar suite passes (248 library / 3 binary / 115 compatibility; two synthetic benchmarks remain ignored), including the mixed-cookie regression; warnings-denied Clippy, root/Sidecar formatting, and `git diff --check` pass. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-128: unauthenticated API-token login submissions are limited to 10 per TCP peer per 60 seconds; excess requests receive 429/`Retry-After`, and a successful token login clears the bucket. | The locked offline Sidecar suite passes (251 library / 3 binary / 116 compatibility; two synthetic benchmarks remain ignored), including login throttling/reset, per-peer isolation/expiry, bounded state, and exact-route regressions; warnings-denied Clippy, root/Sidecar formatting, and `git diff --check` pass. WebUI tests (9), lint, and production build also pass, including accurate retry-delay messaging. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-129: native peer admission, global/per-IP rejections, global connection-budget rejects, handshake read errors, timeouts, and malformed protocol handshakes now appear in `/metrics` without peer labels. | Locked offline targeted tests pass: `rt-engine` ingress (9), listener (5), and EngineHandle (1); `rt-api-native` metrics renderer (1). Warnings-denied Clippy for `rt-engine`/`rt-api-native`, root/Sidecar formatting, and `git diff --check` pass. | No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-130: reconciled the historical hardening handoff so stale branch caveats and already-completed integration steps are clearly superseded by the current ledger. | The handoff's dated state and completion notes now match the canonical burn-down; `git diff --check` passes. | Documentation-only change; no runtime behavior or external qualification claim changed. |
| 2026-09-21 UTC | Closed TNG-131: each `ExecuteBatched` command now has a savepoint, so an error or panic rolls back its partial writes while successful siblings commit; failed savepoint isolation aborts the outer batch. | The regression reproduced a committed row after both write-then-error and write-then-panic before the fix. Locked offline `rt-engine` DbWorker tests pass (22), including per-command rollback and deferred-constraint commit rollback; warnings-denied Clippy, formatting, and `git diff --check` pass. | Current production batched callsites are single-row updates; this closes the worker's generic partial-write failure contract. No public-network request, capacity proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-132: native `/metrics` now exposes database-worker queue depth/capacity, command outcomes/backpressure, and cumulative queue/command/batched-transaction durations without identifying labels. | Locked offline `rt-engine` DbWorker tests (22), direct EngineHandle snapshot test (1), and native metrics renderer test (1) pass; warnings-denied Clippy for `rt-engine`/`rt-api-native`, root/Sidecar formatting, and `git diff --check` pass. | Metrics include cumulative durations for averages, not histogram/tail percentiles. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-133 for retained entries: the primary path-backed scheduler and `StorageRuntime` derive cache capacity from the lazily computed RLIMIT-based 60% ceiling, with a 65,536-entry maximum. | Locked offline `rt-storage` fd-limit tests (5) and file-pool tests (7) pass, including low limits, `RLIM_INFINITY`, scheduler wiring, and zero-capacity behavior; warnings-denied Clippy for `rt-storage`/`rt-engine`/`rt-api-native`, root/Sidecar formatting, and `git diff --check` pass. | TNG-135/136 add per-cache active leases; TNG-138 later adds a shared managed-storage quota. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-134: checkpoint sync opens and fdatasyncs cached/dirty paths sequentially instead of holding one descriptor per dirty file. | The pre-fix child-process test reproduced `EMFILE` at `RLIMIT_NOFILE=64` with 96 dirty files and a one-entry cache; after the fix, both locked offline `rt-storage` sync regressions (2) pass, as do the fd-limit (5) and file-pool (7) tests. Warnings-denied Clippy for `rt-storage`/`rt-engine`/`rt-api-native`, root/Sidecar formatting, and `git diff --check` pass. | TNG-135 now also bounds active leases per path-backed scheduler `FilePool`; separate pools/caches are not combined. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-135: each path-backed scheduler `FilePool` now holds a descriptor permit through cache, caller, and backend ownership, with backpressure after eviction and caller cancellation. | Locked offline `rt-storage` library suite passes (208), including zero-capacity behavior, wait/retry after evicted in-flight ownership, queued-job cancellation, pending `io_uring` completion, and 72 concurrent opens under child-process `RLIMIT_NOFILE` at most 64 with capacity eight; warnings-denied `cargo clippy --offline --locked -p rt-storage --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` pass. | At initial closure this bounded each `FilePool`; TNG-138 later adds a shared quota across managed caches. Unrelated descriptors remain outside that quota. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-136: `StorageRuntime::HandleCache` now keeps its descriptor permit through cache, operation, and backend-job ownership; async admission waits without blocking a Tokio worker. | Locked offline `rt-storage` library suite passes (211), including executor-progress while waiting, evicted in-flight lease release, queued-job cancellation, pending `io_uring` ownership, and 72 concurrent opens under child-process `RLIMIT_NOFILE` at most 64 with capacity eight. Warnings-denied `cargo clippy --offline --locked -p rt-storage --all-targets -- -D warnings`, `cargo check --offline --locked -p rt-api-native`, `cargo fmt --all -- --check`, and `git diff --check` pass. | At initial closure this applied a local per-cache cap; TNG-138 later adds a shared quota across managed caches. Unrelated descriptors remain outside that quota. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-137: engine stats now count shared scheduler file-pool/worker-queue snapshots once per resource identity and export cached-entry and active-descriptor gauges separately. | Locked offline targeted `rt-engine` aggregation and `rt-api-native` metrics tests pass; warnings-denied Clippy, formatting, and `git diff --check` pass. | Shared gauges are deduplicated per managed resource set; this does not create a process-wide fd limit. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-138: scheduler and runtime cache leases now draw from one 60%-of-soft-`RLIMIT_NOFILE` managed-storage quota with cross-cache synchronous/async wakeups and aggregate use/capacity/backpressure metrics. | Locked offline `rt-storage` process-budget tests pass, including a child constrained to `RLIMIT_NOFILE` at most 64 and a two-cache waiter; native metrics renderer exposes the global active/capacity/waits series. Warnings-denied Clippy, formatting, and `git diff --check` pass. | The quota covers `rt-storage` descriptor leases, not sockets or unrelated process descriptors. No public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-139: IPv6 DHT pending peer-forward admission now applies the IPv4 global retained-peer and pending-torrent caps before creating state. | Locked offline targeted `rt-engine` IPv6 regression passes; warnings-denied Clippy, formatting, and `git diff --check` pass. | Local DHT state regression only; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-140: shared native/qBittorrent CSRF admission now fails closed for every present Fetch Metadata value except case-insensitive `same-origin`. | Locked offline `rt-api-model` tests (23), native/qBittorrent facade suites, warnings-denied Clippy, root/sidecar formatting, and `git diff --check` pass; sidecar auth suite passes (252). | This closes local Fetch Metadata authorization parity; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-141: native, qBittorrent, Transmission, Deluge, and sidecar Bearer parsing now accepts case-insensitive schemes with an exact two-token shape through shared/parity helpers. | Locked offline `rt-api-model` (23), native (99), qBittorrent (91), Transmission (45), Deluge (30), and sidecar (252) tests pass; warnings-denied Clippy, formatting, and `git diff --check` pass. | This closes local daemon/facade authentication-parser parity; no public-network request, torrent-count proof, or soak was run. |
| 2026-09-21 UTC | Closed TNG-142: qBittorrent auth and native idempotency public exceptions now match exact registered auth paths rather than suffixes. | Locked offline qBittorrent router regression and native facade suite pass; warnings-denied Clippy, formatting, and `git diff --check` pass. | This closes local public-path allowlist drift; no public-network request, torrent-count proof, or soak was run. |

## Release gate

The TorrentNG-client release gate must fail while any P0 item is Open or while TNG-025,
TNG-026, or TNG-028 is Open. A production-scale claim additionally requires
TNG-010, TNG-013, and TNG-014 to be Resolved with release-artifact evidence.
