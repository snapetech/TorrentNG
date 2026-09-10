# TorrentNG Project Gap Audit

Status as of 2026-09-10 on clean `main` at `b393eb0`; the current release
smoke, public-soak finalization, and kspls0 LVM evidence are tied to this
source revision where stated below.

This audit separates local implementation gaps from external evidence gates.
It is based on the roadmap docs, compatibility matrices, certification status,
and the local checks listed at the end.

The dated audit narrative below is retained for traceability. The current
repository disposition is the 2026-09-04 reconciliation in
[`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md): all repository-
actionable implementation, contract, security, CI, and local evidence work is
closed for its declared scope. Hosted CI is now green. Remaining rows require
broader public clients/networks, additional physical targets,
branch-protection settings, or optional scale/profiling work and are not hidden
implementation backlog.

## Executive Summary

The native engine, storage hot path, memory/resource governor, WebUI build, and
local deterministic API compatibility gates are green. The remaining work is
not concentrated in storage anymore. It is concentrated in release evidence and
compatibility depth:

- the local Docker universal-live interop leg and one official public-swarm
  transfer have passing evidence; the counted public Debian soak and the
  kspls0 real-device storage gate now also pass. Broader public sources and
  additional hardware remain opt-in qualification depth;
- the migration corpus gate now has checked-in generated fixtures for every
  legacy client family and passes strict local validation; adding real exported
  corpora remains optional release-depth evidence for undocumented variants;
- facade compatibility now has native-backed qBit, Transmission, Deluge, and
  rTorrent field projection for the local matrix; remaining compatibility depth
  is live-client behavior and plugin effects that require external clients or a
  deliberate TorrentNG-native workflow owner;
- uTP is implemented at the application transport layer for outbound
  peer-wire, incoming peer-wire when explicitly enabled, and magnet metadata
  fetch; remaining uTP work is public/live interop evidence and operational
  tuning rather than a hidden packet-codec-only implementation gap;
- the current native and sidecar security reviews pass their configuration,
  token, and script-policy checks; rendered-secret, proxy, metrics-exposure,
  dependency, and image-scan review remains deployment/operator evidence;
- the completed public Debian soak is recorded as PASS in both its finalization
  report and the refreshed external preflight; the preflight now recognizes a
  completed PASS report instead of treating the absence of a live process as a
  warning.

## Certification Snapshot

Current `scripts/certification_status.sh` highlights:

| Area | Status |
| --- | --- |
| Native engine rewrite | PASS |
| Hosted CI repository gate | PASS (`34510889406`, all 10 jobs; CodeQL `34510889093` also green) |
| Local release gate | Prior local PASS_WITH_WARNINGS report; final clean b393 release smoke is being refreshed after this evidence commit |
| Storage hardware matrix | PASS on kspls0 LVM (`b393eb0`) |
| Storage io_uring capability/graduation | PASS on kspls0 LVM (`b393eb0`) |
| Storage move/import | PASS on kspls0 LVM (`b393eb0`) |
| Storage release certification | PASS on kspls0 LVM (`b393eb0`) |
| Storage indexed evidence | PASS for current kspls0 LVM reports |
| Security review and scan | PASS |
| Pre-engine release gate | PASS |
| Post-soak release gate | PASS_WITH_WARNINGS; broader optional rows and the prior local warning remain separate |
| Certification burndown | PASS_WITH_ACTIONS while warning rows remain |
| Release readiness | FAIL until every status row is clean PASS/INFO |
| Certification bundle | Generates a hashed archive of latest evidence reports |
| Release evidence suite | Fails until strict readiness passes, while refreshing bundle/burndown |
| Certification JSON status | Machine-readable status export for CI/release automation |
| Universal compatibility | PASS_WITH_SKIPS; current b393 Docker, mobile, and public Debian legs pass, while target storage is certified separately |
| Universal live compatibility | PASS_WITH_SKIPS; canonical b393 all-live report passes local Docker, mobile, and public Debian legs, with the real-device wrapper explicitly skipped |
| Migration corpus | PASS with generated checked-in corpus; strict local gate passes |
| External evidence preflight | PASS in strict mode for Docker, public opt-in, writable storage target, corpus, and completed soak |
| 24h soak | PASS; 1,437 samples and exact completed public torrent |

## Roadmaps

`docs/ROADMAP.md` and `docs/ENGINE_REWRITE_BURNDOWN.md` are mostly closed for
native implementation. The remaining roadmap risk is that the high-level
roadmap now mixes completed implementation claims with evidence boundaries from
the compatibility matrix.

Remaining qualification gates:

- `docs/CLIENT_COMPATIBILITY_MATRICES.md` and `docs/INTEROP_MATRIX.md` retain
  optional live-client, public-network, and broader protocol qualification rows;
  they are not unassigned native implementation work.
- The 24-hour soak remains an explicit release-evidence gate. The named Debian
  run is complete; future release artifacts still need their own operator-owned
  soak when the runtime artifact or configuration materially changes.

## Storage

Storage implementation is closed locally. The current live path includes
bounded positioned I/O, fd pooling, preallocation, durability barriers,
dedicated disk/hash workers, peer-read readahead, HDD elevator, topology
detection, sparse recheck, move/import/delete planning, storage-plan jobs, and
release certification wrappers. Native REST now also exposes
`GET /api/v1/storage` directly from the engine storage-root registry with live
capacity probes, so WebUI/native deployments no longer depend on sidecar-only
storage status projection.

Current kspls0 LVM evidence is complete for the exercised target:

- storage hardware, io_uring, move/import, LVM extent sampling, and the
  real-device matrix all pass against `/dev/mapper/datapool_lvm-media` at
  commit `b393eb0`;

Remaining storage qualification depth is evidence-bound:

- HDD 5x wall-clock claims require a run on an HDD target with
  `TNG_STORAGE_REQUIRE_HDD_5X=1`.
- LVM/PV placement claims require an LVM target with extent probing enabled.
- Making `io_uring` an automatic default requires target-hardware graduation
  evidence proving selected `uring`, registered files, registered frame slots,
  and throughput against the `pread` baseline.
- Multi-TB move/import claims require operator-sized real-root fixture runs.

## Memory

Memory/resource-governor work is locally green:

- queued-disk leases fail closed before enqueue;
- storage frames, peer buffers, piece assembly, API snapshots, tracker peers,
  DHT table, metadata, webseed bodies, and queued disk work are accounted;
- 100k idle and 1k hot-seeding proxy rows pass through the local release report;
- hash/recheck isolation and peer-read backpressure are covered by scale tests.

Remaining memory work is evidence-bound:

- the completed public soak covers the native interop configuration; a future
  materially different release config still needs its own soak;
- fleet-size claims still depend on live deployment measurements, not just
  deterministic proxy tests.

## WebUI

The WebUI is implemented and builds:

- virtualized torrent table, server-side filtering/sorting, WebSocket/delta
  hooks, bulk edit dialogs, tracker health, ratio groups, storage planner,
  saved views, mobile-safe controls, logs, RSS rules, workflows, appearance,
  and engine/storage panels exist in `webui/src`.
- The top bar and status bar consume `/health` runtime capabilities for uTP, so
  operators can see whether peer-wire, metadata, or incoming uTP paths are
  actually active rather than inferring from implementation support alone.
- `npm run build` passes.
- `npm run lint` passes.

WebUI browser certification now has a local gate:

```sh
scripts/webui_certification.sh
```

This runs the production build, lint, and a mocked-API Playwright matrix across
desktop and mobile viewports. The browser matrix checks first paint, table
rendering, selection state, settings navigation, storage panel rendering,
15k-row virtualized table behavior, core accessible control names, automated
axe WCAG structural checks, deterministic visual-regression baselines for the
main workspace and storage settings panel, and console/page-error cleanliness.
`scripts/local_release_gate.sh` now runs the same WebUI certification as part
of the local release path. It also runs the migration corpus gate against the
checked-in generated corpus so strict local validation has artifact coverage
for every supported source family.

Remaining WebUI gaps are now product/certification depth:

- visual-regression screenshot baselines are wired into the WebUI browser
  matrix for the main workspace and storage settings panel;
- axe-based accessibility certification is wired in for serious/critical WCAG
  violations, including color contrast on the certified workspace/settings
  surfaces;
- the browser-driven 15k-row benchmark now verifies bounded DOM rendering,
  load-more responsiveness, and a configurable first-visible threshold through
  `TNG_WEBUI_FIRST_VISIBLE_MS` in `scripts/webui_certification.sh`;
- some plugin panels intentionally show compatibility-state surfaces until
  TorrentNG owns native plugin workflows such as blocklist, execute, extractor,
  scheduler, or auto-add behavior.

## API And Compatibility

Local deterministic API compatibility is passing:

- qBittorrent, Transmission, Deluge, and rTorrent facade certification passed
  via `scripts/api_facade_certification.sh`.
- `scripts/universal_compatibility_certification.sh` passed for local
  deterministic coverage. When optional live legs are not enabled, the report
  status is `PASS_WITH_SKIPS` instead of plain PASS. The live mobile qBittorrent
  read matrix is now an explicit optional universal gate through
  `UNIVERSAL_COMPAT_MOBILE=1`. The current targeted kspls0 report records the
  real-device storage leg as PASS; it intentionally skips unrelated Docker and
  public invocations.

Remaining compatibility depth:

- Transmission: JSON-RPC 2.0 method errors, stateful notification subscription
  probes, broad mutable session settings, group limit state roundtrips, and
  aggregate native peer rates are covered in the facade matrix. ETA now projects
  from native peer rates, and tracker stats project persisted engine announce
  state, including timestamps, status messages, and scrape counts; true push
  notification delivery and native group scheduling effects remain future
  live-client parity work.
- Deluge: extractor, scheduler, execute, blocklist, and autoadd plugin-specific
  APIs now have structured compatibility surfaces with safe no-op mutations;
  torrent peer/rate fields and tracker status fields project native snapshots
  when available; remaining plugin work is native behavioral effects only where
  TorrentNG explicitly chooses to own those workflows.
- rTorrent: file/tracker/peer multicalls now project native metadata, persisted
  tracker state, and peer snapshots when an engine is attached, with registry
  fallback file rows for in-memory compatibility probes. Global throttle reads
  use native limits where available; common view sizes are registry-backed; and
  custom views round-trip through `view.add`/`view.set` with registry size
  projection. Deeper per-view filter expressions remain compatibility depth.
- qBittorrent: common automation flows are covered, and `torrents/info`,
  `sync/maindata`, `transfer/info`, `torrents/files`, and
  `torrents/trackers` now project native peer snapshots, per-file progress,
  aggregate rates, and persisted tracker status/messages/counts where
  available. Remaining depth is live-client presentation parity for
  client-specific edge cases.
- DHT-only magnets: DHT `get_peers` forwarding and trackerless BEP 9 metadata
  completion from discovered peers are unit-covered in `rt-engine`; the Docker
  matrix now also includes `rust-trackerless-magnet` for trackerless metadata
  and payload transfer through an explicit peer bridge. Public DHT-only swarm
  discovery remains external release evidence.
- uTP: `rt-utp` provides the packet/state/UDP stream layer, and the native
  engine has policy-gated outbound peer-wire, boolean-gated incoming peer-wire,
  and metadata-fetch paths. `/health` reports the active `utp_transport_paths`
  so operators can distinguish enabled runtime paths from crate capability.
  Remaining depth is public-swarm interop, dashboards, and deployment tuning.
- `scripts/migration_corpus_certification.sh` now separates synthetic
  import/apply coverage from fixture artifact coverage. It runs `rt-migrate`
  tests and scans `testdata/migration-corpus/{qbittorrent,transmission,deluge,
  utorrent,biglybt,tixati,rtorrent,generic}`. The checked-in generated corpus
  and manifest cover every source family and pass with
  `TNG_REQUIRE_MIGRATION_CORPUS=1`.
- Real exported golden fixture corpora can still be added for qBittorrent,
  Transmission, Deluge, uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati,
  rTorrent, and generic bencoded/JSON edge cases when release evidence needs
  undocumented client/version variants beyond generated fixtures.
- `scripts/external_evidence_preflight.sh` uses the same artifact filename
  patterns as `scripts/migration_corpus_certification.sh`, so placeholder
  files such as `README.md` do not satisfy exported-corpus coverage.

## Wire Interop

The deterministic local compatibility certification passes. Skipped live legs
are now explicit in the universal compatibility report and certification status.
The post-soak release rollup now marks `PASS_WITH_GAPS`, `PASS_WITH_SKIPS`,
`PASS_WITH_WARNINGS`, `SKIP`, and stale/running evidence rows as `WARN` instead
of treating them as a clean evidence set.
`scripts/certification_burndown.sh` turns those non-clean status rows into an
action table with the exact commands or artifact drops needed to reach a clean
release report.
`scripts/start_24h_soak.sh` starts the long 24h soak with the correct
`soak-24h-*` report naming, PID file, and log path so status can distinguish an
active run from a stale partial report.
`scripts/universal_live_certification.sh` is the single entry point for the
external universal compatibility legs: Docker client interop by default, with
opt-in public torrent and real-device storage runs.
`scripts/release_readiness_gate.sh` is the strict final gate: it fails on any
non-clean certification row and writes a paired burndown report.
`scripts/certification_bundle.sh` packages the latest certification status and
referenced reports into a hashed `certification/bundles/` tarball for release
notes or handoff.
`scripts/release_evidence_suite.sh` is the one-command strict evidence refresh:
it updates status, burndown, readiness, and bundle reports, and fails while
strict readiness still has blockers.
`docs/RELEASE_EVIDENCE.md` is the runbook for refreshing the evidence bundle
and for the remaining broader qualification rows.
The current full Docker interop matrix has a clean local result in
[`interop-matrix-20260910T190228Z.md`](../certification/reports/interop-matrix-20260910T190228Z.md):
28/28 cases pass against the current image and source tree. The remaining
interop work is external or optional:

- additional public legal torrent sources beyond the passing Debian matrix;
- additional real-device storage targets beyond the passing kspls0 LVM run;
- the Docker protocol matrix now includes `rust-trackerless-magnet`, which adds
  TorrentNG from a trackerless magnet and completes via an explicit bridged
  peer, covering trackerless BEP 9 metadata and payload transfer in the local
  client stack. DHT-only public peer discovery remains a stricter public/live
  evidence row because it depends on swarm reachability and deployment network
  policy. `tracker-outage-after-peer-discovery`, `webseed-outage-fallback`,
  `endgame-multi-peer`, `private-torrent-no-dht-pex`,
  `rust-seeds-to-all-reference-clients`, `resume-after-partial-download`,
  `force-recheck-corruption-repair`, and `missing-file-recovery` are now
  implemented Docker protocol rows covering tracker outage after peer discovery,
  webseed outage fallback, multi-peer completion, private torrent DHT/PEX
  policy, TorrentNG as sole seeder to all reference clients, preseeded partial
  restart/resume, corrupt payload repair, and deleted payload recreation with
  final hash verification;
- expansion backlog for DHT/PEX/LSD, multi-tracker tiers, file layout edge
  cases, network adversity, stress, and seeding behavior.

## Security

The repository security checks pass for the current native and sidecar
configuration fixtures. Native metrics identifiers are hashed by default;
sidecar metrics requires credentials when tokens are configured; public binds
require strong tokens and a signing secret; and proxy-header trust is
loopback-only. The remaining checks are deployment-specific rather than open
source work.

Release-operator checks before shipping:

- run `scripts/security_review.sh` against the selected config;
- confirm scripts are disabled or constrained to explicit non-world-writable
  directories;
- confirm API tokens are non-example values;
- confirm trusted proxy header mode is only enabled behind a proxy that strips
  spoofed inbound headers;
- confirm `/metrics` exposure is internal-only or protected.

## Packaging And Operations

Native deployment docs and packaging artifacts exist for systemd, Docker,
Compose, Kubernetes, Prometheus/Grafana, and Arch/AUR template coverage.

Remaining external operational evidence:

- rerun the release suite against the exact release config when the release
  artifact changes materially;
- attach security, storage, compatibility, and soak reports to release notes;
- review branch-protection enforcement and qualify additional public/device
  targets if the product claim requires them.

## Historical Validation Run

Commands run successfully:

```sh
scripts/certification_status.sh
cd webui && npm run build
cd webui && npm run lint
scripts/webui_certification.sh
scripts/api_facade_certification.sh
scripts/migration_corpus_certification.sh
scripts/universal_compatibility_certification.sh
```

The universal compatibility report passed but explicitly skipped the Docker
client interop, public torrent interop, and real-device storage legs unless
their enabling environment variables are set.
