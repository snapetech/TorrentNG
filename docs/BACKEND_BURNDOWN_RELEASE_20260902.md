# TorrentNG Backend Audit Burn-down — Release Evidence

## Current artifact and qualification evidence (2026-09-10 UTC)

| Check | Result | Evidence |
| --- | --- | --- |
| Source revision | recorded | current pushed source is `main` at `b393eb0`; the release binary and storage reports below identify their exact source revision |
| Release build | PASS | `cargo build --release --locked -p torrentngd` |
| Binary | PASS | `target/release/torrentngd`; 22,448,768 bytes; SHA-256 `0a8233db7d34c17854b5ee20aa62abad33ec34b36444d68ea714d6b801836067` |
| Authenticated deployment smoke | PENDING FINAL SMOKE | A clean release-binary smoke will be recorded after this evidence reconciliation commit |
| Startup/health | PASS | Release binary started from the isolated authenticated config and returned ready health |
| Native list/transfer | PASS | Native list envelope and aggregate transfer-info requests returned HTTP 200 |
| qBittorrent list/transfer | PASS | qBittorrent list and aggregate transfer-info requests returned HTTP 200 |
| Prometheus metrics | PASS | 647 lines / 43,459 bytes returned HTTP 200 |
| SIGTERM | PASS | Clean exit observed after 2 polls; total smoke duration 462 ms |
| Compose render | PASS | `docker compose -f deploy/native/compose.yml config --quiet` |
| Live fault matrix | PASS | [`backend-burndown-native-fault-live-current-20260904.md`](../certification/reports/backend-burndown-native-fault-live-current-20260904.md); SIGKILL/restart, SQLite failure/recovery, storage cancellation, and filesystem failure all remain isolated |
| API/SSE load | PASS | [`backend-api-load-current-20260904-final.md`](../certification/reports/backend-api-load-current-20260904-final.md); 204,936 requests over 30 seconds, 32 JSON clients, 8 slow SSE consumers, zero errors |
| Local client/protocol interoperability | PASS | [`interop-matrix-20260910T190228Z.md`](../certification/reports/interop-matrix-20260910T190228Z.md); 28/28 local cases across qBittorrent, Transmission, Deluge, and rTorrent at `b393eb0` |
| Public legal torrent matrix | PASS | [`interop-matrix-20260910T192200Z.md`](../certification/reports/interop-matrix-20260910T192200Z.md); official Debian 13.6 netinst, 791,674,880 bytes, Rust completed, 142 Rust peers across five clients |
| Public Debian 24-hour soak | PASS | [`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md); 1,437 samples, exact completed torrent, health/resource checks pass |
| Canonical universal compatibility | PASS_WITH_SKIPS | [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md); static, migration, local Docker, mobile, and public Debian legs pass; real-device storage is a separate targeted report |
| Security scan | PASS | [`security-scan-current-20260904.md`](../certification/reports/security-scan-current-20260904.md); npm audit, locked Cargo tree, and container scan pass with no HIGH/CRITICAL findings |
| kspls0 LVM storage certification | PASS | [`storage-release-certification-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/storage-release-certification-kspls0-lvm-20260910-b393eb0.md); exact `b393eb0`, HDD median ratio 5.11x, LVM extent probe, io_uring, and real-root move/import pass |
| kspls0 real-device storage matrix | PASS_WITH_SKIPS | [`universal-live-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/universal-live-kspls0-lvm-20260910-b393eb0.md); storage leg PASS; Docker/public legs were intentionally skipped in this targeted run |
| External evidence preflight | PASS | [`external-evidence-preflight-release-strict-20260910-b393eb0.md`](../certification/reports/external-evidence-preflight-release-strict-20260910-b393eb0.md); strict preflight has no warnings |

Hosted repository gates are current for the pushed product revision: run
`34510889406` passed all ten jobs and dynamic CodeQL run `34510889093` passed
all four analyses on `b393eb0`. Branch-protection enforcement is still a
separate repository setting.

This is the current local release-binary record plus the current public Debian
transfer, completed soak, and kspls0 LVM qualification results. It is not a
universal-compatibility or 100k-capacity certificate. The counted soak source
predates the b393 source refresh; the current b393 public transfer and storage
reports are separate artifact-specific evidence. The older release bundle below
is retained as historical evidence.

Everything below is historical evidence from the 2026-09-02 bundle. Its old
TNG-011/TNG-029 wording and artifact digest are superseded by the current
reconciliation and release record in
[`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md).

## Historical 2026-09-02 bundle (superseded)

Date: 2026-09-02  
Source commit: `8b00722`
Worktree: dirty at build time  
Release status: **BLOCKED for production-scale and universal-compatibility claims**

This is the evidence bundle index for the TNG-010, TNG-011, TNG-013, TNG-026,
and TNG-029 remediation pass. The code is buildable and the native daemon was
started from the release artifact, but the external acceptance envelope is not
complete. A local green gate is not a production release certificate.

## Artifact and deployment run

| Check | Result | Evidence |
| --- | --- | --- |
| Release build | PASS | `cargo build --release --locked -p torrentngd` completed in 25.46 s |
| Binary | PASS | `target/release/torrentngd`; 18,739,248 bytes; SHA-256 `ac7fc55c74bb8dffb63b24914ed9e2004b7d9dee3a24c80abf3587b66e5f06da` |
| Config | PASS | [`backend-burndown-native-config-20260902.toml`](../certification/reports/backend-burndown-native-config-20260902.toml); isolated `/tmp` session/data roots; token-authenticated |
| Startup/deploy | PASS | [`backend-burndown-native-release-smoke-final-20260902.md`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.md) and [`backend-burndown-native-release-smoke-final-20260902.log`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.log) |
| Native health | PASS | [`backend-burndown-native-release-smoke-final-20260902.health.json`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.health.json); HTTP 200, ready, storage workers healthy |
| Native list | PASS | [`backend-burndown-native-release-smoke-final-20260902.torrents.json`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.torrents.json); HTTP 200, snapshot envelope |
| Native transfer info | PASS | [`backend-burndown-native-release-smoke-final-20260902.transfer.json`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.transfer.json); HTTP 200, aggregate stats path |
| qBittorrent list | PASS | [`backend-burndown-native-release-smoke-final-20260902.qbit.json`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.qbit.json); HTTP 200 |
| qBittorrent transfer info | PASS | [`backend-burndown-native-release-smoke-final-20260902.qbit-transfer.json`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.qbit-transfer.json); HTTP 200, aggregate stats path |
| Prometheus metrics | PASS | [`backend-burndown-native-release-smoke-final-20260902.metrics.txt`](../certification/reports/backend-burndown-native-release-smoke-final-20260902.metrics.txt); HTTP 200, 611 lines / 41,287 bytes, including worker-health and API-pressure series |
| SIGTERM shutdown | PASS | Runtime log shows signal receipt, clean torrent-task shutdown, and engine shutdown; external poll completed in 2 attempts; smoke duration 250 ms |
| 100k production-daemon scale | PASS_WITH_LIMITATIONS | [`backend-burndown-native-scale-release-final-20260902.md`](../certification/reports/backend-burndown-native-scale-release-final-20260902.md); 100,000 file-backed rows, 217 ms to health, 120,434,688 bytes RSS after restore, native page-1 33.131 ms, restart, promotion/demotion, and active-task gauge |

The deployment was a direct local release-binary run, not a Docker, systemd,
Kubernetes, public-network, real-device, or long-soak deployment.

## Gate results

| Gate | Result | Evidence |
| --- | --- | --- |
| Local release gate | `PASS_WITH_WARNINGS` | [`local-release-backend-burndown-final-20260902.md`](../certification/reports/local-release-backend-burndown-final-20260902.md) |
| Storage NG feature matrix | PASS | Included by local release gate |
| WebUI certification | PASS | [`webui-certification-20260902T174313Z.md`](../certification/reports/webui-certification-20260902T174313Z.md) |
| API facade certification | PASS | [`api-facades-local-release-20260902T174323Z.md`](../certification/reports/api-facades-local-release-20260902T174323Z.md) |
| Migration corpus | PASS | [`migration-corpus-local-release-20260902T174331Z.md`](../certification/reports/migration-corpus-local-release-20260902T174331Z.md) |
| Native/sidecar config security review | PASS | [`security-review-native-local-20260902T174333Z.md`](../certification/reports/security-review-native-local-20260902T174333Z.md), [`security-review-sidecar-local-20260902T174333Z.md`](../certification/reports/security-review-sidecar-local-20260902T174333Z.md) |
| External preflight | `PASS_WITH_WARNINGS` | [`external-evidence-preflight-backend-burndown-final-20260902.md`](../certification/reports/external-evidence-preflight-backend-burndown-final-20260902.md) |
| Strict readiness | FAIL | [`release-readiness-backend-burndown-final-20260902.md`](../certification/reports/release-readiness-backend-burndown-final-20260902.md) |
| Local-scope readiness | PASS | [`release-readiness-local-backend-burndown-final-20260902.md`](../certification/reports/release-readiness-local-backend-burndown-final-20260902.md) |

The local gate’s warning is real: its storage release certification leg is
skipped. Local-scope readiness passes because it ignores explicitly external
opt-in rows and the stale 24-hour soak, while strict readiness remains blocked
by stale/skipped universal compatibility, external warnings, and the stale soak
row inherited from the report directory. The status script excludes
`universal-live-soak-ready.md` because that file is an intermediate readiness
note rather than a certification result. The external preflight reports three
warnings: public transfer was not opted in, no real-device storage target was
configured, and no 24-hour soak was active.

## Focused finding disposition

| Finding | Current result | What is still not proven |
| --- | --- | --- |
| TNG-010 | Runtime tier path, compact dormant registry projections, persisted tracker-deadline promotion, and incremental tier counts are implemented; **local functional gate complete** | 1k/2k simultaneous-hot production metadata evidence is deferred capacity proof. The older 100k file-backed report remains historical. |
| TNG-011 | Bounded background worker, end-to-end in-flight cap, dedicated worker DB connection, durable recovery, rich terminal completion, `commit_pending` finalization, bounded retries, saturation gauges/alerts, and restart reconciliation are implemented; **local functional and live fault gate complete** | Hosted/device deployment evidence and broader disk/permission/space permutations remain external reliability proof |
| TNG-013 | Revision snapshots, bounded pages, indexes, journal deltas, incremental refresh, maintained counters, aggregate stats, bounded SSE, pressure telemetry, qBit large-output guards, and bounded sidecar qBittorrent page sync are implemented; **local functional and many-client/slow-consumer gate complete** | Representative production corpus, allocator profile, and public/client load remain external load proof; external qBittorrent paging is eventual because its API has no server-side snapshot |
| TNG-026 | Current release artifact was built, deployed locally, authenticated, smoked, and shut down cleanly; **local deployment gate complete** | Public/client/device/24-hour evidence and strict readiness remain external gates |
| TNG-029 | Storage, policy, registry, API, budget, capability, dependency-health, bounded command, task-reaping, tracker, peer-listener, and supervised persistence seams are implemented; **stated persistence-isolation gate complete** | Full actor decomposition is a non-release maintainability choice; deployment-specific fault evidence remains external |

## Required actions before release claims

| Owner | Action | Required artifact / gate |
| --- | --- | --- |
| Release / interoperability | Extend the passing Debian public run to the remaining approved public sources and refresh universal compatibility against this artifact | New `universal-compat-*` and `universal-live-*` reports with no unknown/stale rows |
| Storage operator | Set `TNG_STORAGE_BENCH_DIR` to the target mount and run the real-device storage certification | New storage hardware, io_uring, move/import, and indexed reports |
| Release operator | Finalize the active named Debian soak after 86,400 seconds | `.run/soak-24h-public-debian-20260905-v3.md` with the required sample count and a passing post-soak gate |
| Backend performance | Optional extended proof: run 1k/2k simultaneous hot fixtures with real metadata diversity, peer traffic, and tracker deadlines | The older [`backend-burndown-native-scale-release-final-20260902.md`](../certification/reports/backend-burndown-native-scale-release-final-20260902.md) is historical; this is not part of the current functional gate |
| Storage reliability | Crash/restart a real move/import plan, including sparse checkpoint indexes, cancellation, disk/permission failure, and DB persistence failure | Durable job/event report plus reconciliation behavior |
| API performance | Optional extended proof: load list, stats, SSE, qBit `sync/maindata`, slow consumers, cursor expiry, and concurrent snapshot refreshes | Latency/allocation/backpressure report; documented large-qBit live-field semantics are already explicit |

Until the active soak and remaining external artifacts finish, the honest
product statement is: **the local release binary and one official public
Debian transfer pass; 24-hour, universal-compatibility, real-device, 100k
capacity, and production deployment readiness are not certified.**
