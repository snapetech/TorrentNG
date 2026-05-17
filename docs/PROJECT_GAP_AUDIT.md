# TorrentNG Project Gap Audit

Status as of 2026-05-17 on `main`.

This audit separates local implementation gaps from external evidence gates.
It is based on the roadmap docs, compatibility matrices, certification status,
and the local checks listed at the end.

## Executive Summary

The native engine, storage hot path, memory/resource governor, WebUI build, and
local deterministic API compatibility gates are green. The remaining work is
not concentrated in storage anymore. It is concentrated in release evidence and
compatibility depth:

- live Docker client interop and public-swarm runs are still release evidence
  gates, not covered by the default local compatibility run;
- real exported migration corpora are still needed for all legacy clients;
- facade compatibility still has placeholder-depth areas for live
  peer/tracker/webseed details and some client-specific plugin APIs;
- the security release checklist is intentionally unchecked until run against
  the exact deployment config;
- the 24h soak status row is `RUNNING/UNKNOWN` in the latest status report,
  even though short, transfer-churn, finalization, local release, and post-soak
  gates are passing.

## Certification Snapshot

Current `scripts/certification_status.sh` highlights:

| Area | Status |
| --- | --- |
| Native engine rewrite | PASS |
| Local release gate | PASS |
| Storage hardware matrix | PASS |
| Storage io_uring capability/graduation | PASS |
| Storage move/import | PASS |
| Storage release certification | PASS |
| Storage indexed evidence | PASS |
| Security review and scan | PASS |
| Pre-engine and post-soak release gates | PASS |
| 24h soak | RUNNING/UNKNOWN |

## Roadmaps

`docs/ROADMAP.md` and `docs/ENGINE_REWRITE_BURNDOWN.md` are mostly closed for
native implementation. The remaining roadmap risk is that the high-level
roadmap now mixes completed implementation claims with evidence boundaries from
the compatibility matrix.

Actionable gaps:

- Keep `docs/CLIENT_COMPATIBILITY_MATRICES.md` as the live backlog for broad
  ecosystem compatibility. It still has P1/P2 rows for live client matrices,
  public transfer interop, storage resume scenarios, golden import corpora, and
  plugin auxiliary APIs.
- Keep `docs/INTEROP_MATRIX.md` as the live backlog for protocol and
  client-to-client evidence. Several protocol rows remain planned.
- Decide whether `24h soak` should be finalized, superseded by transfer-churn
  soak, or removed from release status if the report is stale.

## Storage

Storage implementation is closed locally. The current live path includes
bounded positioned I/O, fd pooling, preallocation, durability barriers,
dedicated disk/hash workers, peer-read readahead, HDD elevator, topology
detection, sparse recheck, move/import/delete planning, storage-plan jobs, and
release certification wrappers.

Remaining storage work is evidence-bound:

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

- production-scale soak evidence should be refreshed for the exact release
  config;
- the current 24h soak report is still `RUNNING/UNKNOWN` in status output;
- fleet-size claims still depend on live deployment measurements, not just
  deterministic proxy tests.

## WebUI

The WebUI is implemented and builds:

- virtualized torrent table, server-side filtering/sorting, WebSocket/delta
  hooks, bulk edit dialogs, tracker health, ratio groups, storage planner,
  saved views, mobile-safe controls, logs, RSS rules, workflows, appearance,
  and engine/storage panels exist in `webui/src`.
- `npm run build` passes.
- `npm run lint` passes.

Remaining WebUI gaps are product/certification depth:

- no Playwright or browser screenshot matrix is wired into the repo for
  responsive layout, accessibility, or visual regression;
- WebUI performance targets in the roadmap are represented by backend/API scale
  tests, but not by a browser-driven 15k-row render benchmark in CI;
- some panels necessarily reflect compatibility placeholder depth from backend
  facades, especially live peer/tracker/webseed activity.

## API And Compatibility

Local deterministic API compatibility is passing:

- qBittorrent, Transmission, Deluge, and rTorrent facade certification passed
  via `scripts/api_facade_certification.sh`.
- `scripts/universal_compatibility_certification.sh` passed for local
  deterministic coverage.

Remaining compatibility gaps:

- Transmission: deeper 4.1 native parity beyond the compatibility envelope
  remains P1, especially exact error codes, notifications, group internals, and
  native-backed effects for script/blocklist/preferred transport settings.
- Deluge: extractor, scheduler, execute, blocklist, and autoadd plugin-specific
  APIs remain gaps unless a target migration/client requires them.
- rTorrent: file/tracker/peer multicalls are stable compatibility shapes, but
  live detail remains placeholder-depth until native snapshots expose equivalent
  file/tracker/peer detail.
- qBittorrent: common automation flows are covered; deeper live tracker delta
  fidelity and live swarm availability counters remain placeholder-depth.
- Real exported golden fixture corpora are still needed for qBittorrent,
  Transmission, Deluge, uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati, and
  rTorrent.

## Wire Interop

The deterministic local compatibility certification passes, but the full Docker
interop matrix has explicit release evidence still to run:

- local Docker client-to-client rows across qBittorrent, Transmission, Deluge,
  rTorrent, and TorrentNG;
- public legal torrent matrix;
- planned protocol rows: DHT-only magnet, private torrent DHT/PEX policy,
  force-recheck corruption repair, resume-after-partial-download, endgame
  multi-peer, and TorrentNG as the sole long-running seeder for reference
  clients;
- expansion backlog for DHT/PEX/LSD, multi-tracker tiers, file layout edge
  cases, network adversity, stress, and seeding behavior.

## Security

Security scripts and reports exist and the current status shows PASS, but
`docs/SECURITY_REVIEW.md` intentionally leaves the release checklist unchecked
because it must be run against the exact release deployment config.

Release-blocking checks before shipping:

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

Remaining operational evidence:

- rerun the release suite against the exact release config and target hardware;
- attach security, storage, compatibility, and soak reports to release notes;
- resolve the stale or still-running 24h soak row.

## Validation Run During This Audit

Commands run successfully:

```sh
scripts/certification_status.sh
cd webui && npm run build
cd webui && npm run lint
scripts/api_facade_certification.sh
scripts/universal_compatibility_certification.sh
```

The universal compatibility report passed but explicitly skipped the Docker
client interop, public torrent interop, and real-device storage legs unless
their enabling environment variables are set.
