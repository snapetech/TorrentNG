# Roadmap

This project has two runtime tracks. Track 1 delivered immediate value on top
of rTorrent and remains a compatibility/migration facade. Track 2 is the native
Rust engine that replaces rTorrent for native deployments and is now the primary
rewrite surface.

---

# Track 1 — rTorrent Sidecar

Fix the rTorrent/ruTorrent pain surface without replacing the engine. Ship something useful now.

## Phase 0 — Audit

Enumerate every known rTorrent 0.16.x + ruTorrent 5.3.x integration breakage.

**Checklist:**
- [x] `load.start` blocked for untrusted connections via httprpc (ruTorrent#3046)
- [x] `load.raw.start` untrusted status
- [x] `d.tracker_announce` untrusted status
- [x] xmlrpc-c vs tinyxml2 RPC erratic behavior (rtorrent#1636)
- [x] XMLRPC parsererror on torrent list (ruTorrent#2977)
- [x] httprpc raw passthrough trust bypass
- [x] *arr add torrent broken flows
- [x] autobrr add torrent broken flows
- [x] ruTorrent 10k+ torrent UI performance (dxSTable regression)
- [x] PHP 8.5 deprecations in ruTorrent 5.3.x
- [x] Plugin permission check breakage in ruTorrent 5.3.1
- [x] Socket permission gotchas (SCGI socket world-readable vs group)
- [x] Large-batch `.torrent` file add limits

Produce: `docs/AUDIT.md` with status per item and workaround/fix notes.

## Phase 1 — Known-good distribution bundle

Ship a pinned, tested, known-good rTorrent + ruTorrent bundle.

**Deliverables:**
- [x] `engine-profile/rtorrent.rc` — known-good config
- [x] `engine-profile/build/` — build scripts (tinyxml2, recommended flags)
- [x] Patched httprpc trust behavior
- [x] `deploy/docker/Dockerfile.phase1` — single-container
- [x] `deploy/docker/compose.phase1.yml` — rTorrent + ruTorrent + nginx
- [x] `scripts/healthcheck.sh` — SCGI/socket/RPC/auth diagnostic
- [x] Integration test suite for *arr and autobrr add-torrent flows
- [x] `docs/MIGRATION.md` — import existing `.rtorrent.rc`, ruTorrent settings

## Phase 2 — Sidecar daemon MVP

The Rust sidecar becomes the control plane. ruTorrent can still coexist.

**Minimum viable API:**
- `GET    /api/v1/torrents` — list with pagination, filter, sort
- `POST   /api/v1/torrents` — add torrent (file or magnet)
- `DELETE /api/v1/torrents/:hash` — remove
- `POST   /api/v1/torrents/:hash/start`
- `POST   /api/v1/torrents/:hash/stop`
- `POST   /api/v1/torrents/:hash/recheck`
- `POST   /api/v1/torrents/:hash/reannounce`
- `GET    /api/v1/torrents/:hash/files`
- `PATCH  /api/v1/torrents/:hash/files`
- `GET    /api/v1/torrents/:hash/trackers`
- `PATCH  /api/v1/torrents/:hash/trackers`
- `GET    /api/v1/settings/user-agent` / `PUT`
- `GET    /ws` — WebSocket delta events
- `GET    /health`
- `GET    /metrics`

**Also:** SQLite state cache, TOML config + env overrides, API token auth, structured JSON logs, Prometheus metrics.

## Phase 3 — qBittorrent API compatibility shim

Make existing *arr/autobrr/tool integrations work by selecting "qBittorrent" as the client type.

**Target endpoints:** auth, app info, torrent CRUD, tracker ops, file priorities, categories, tags, sync/maindata, transfer info. See `docs/API.md`.

**Test suite:**
- Prowlarr/Sonarr/Radarr add-torrent flows
- autobrr add-torrent flow
- cross-seed announce flow
- NZB360 / Transdrone read-only flow

## Phase 4 — Modern WebUI

Replace ruTorrent as the primary UI.

**Priority features:**
1. [x] Virtualized torrent table (100k-row target)
2. [x] Server-side filter + sort
3. [x] WebSocket delta sync
4. [x] Bulk ops with dry-run preview
5. [x] Tracker health view
6. [x] Ratio group management
7. [x] Storage/mount dashboard
8. [x] Saved views
9. [x] Mobile-safe interactions (no right-click required)

## Phase 5 — Workflow platform

Sidecar-managed replacement for high-value ruTorrent plugins.

**Priority workflows:**
- [x] RSS rules + autobrr integration
- [x] Post-complete hooks (webhook, category/path actions, and config-gated script execution)
- [x] Post-complete unpack/hardlink script execution foundation
- [x] Cross-seed helper
- [x] Tracker repair / bulk tracker replace
- [x] Per-tracker ratio policies
- [x] Category/path automation rules
- [x] Webhook actions
- [x] *arr status feedback compatibility surface

## Track 1 benchmark targets

| Scenario | Target |
|---|---|
| 1k torrents — UI first paint | < 1s |
| 10k torrents — UI first paint | < 2s |
| 15k torrents — UI first paint | < 3s |
| 50k synthetic — `/torrents/info` API | < 500ms |
| `/sync/maindata` delta under normal churn | < 50ms |
| Sidecar memory at 15k torrents after 24h | < 500MB |
| Cold start + first torrent list ready | < 5s |

---

# Track 2 — Native Rust Engine

A ground-up Rust BitTorrent daemon optimized for 10k–100k torrents, 200+ TB
libraries, private-tracker seeding, and operational observability. This track is
implemented across the workspace and certified by
`scripts/native_engine_certification_report.sh`.

See `docs/ENGINE.md` for the full design.

## North Star

> A Rust-native, headless-first BitTorrent daemon and compatibility layer that
> can move into, out of, and alongside the major torrent client ecosystems:
> qBittorrent, Transmission, Deluge, rTorrent, uTorrent/BitTorrent Classic,
> BiglyBT/Vuze, Tixati, common automation tools, and real BitTorrent swarms.

The engine is a **massive-library seeding engine** and a **compatibility-first
torrent control plane**. It should be able to import existing state, project the
APIs tools expect, interoperate on the wire, and expose a native model that is
more observable and easier to operate than the historical client-specific
internals it replaces. Native downloading, DHT/uTP protocol crates, and BEP 52
metadata/storage/API support are part of the rewrite surface; streaming remains
outside the first production target.

## Track 2 — Phase 0: Research and design lock

Deliverables:
- BEP compliance matrix
- API compatibility matrix (qBit, Transmission)
- Storage design doc and invariants
- Session DB schema
- Threat model
- Benchmark plan with synthetic 1k/5k/10k/15k/50k datasets
- Migration plan (from rTorrent/qBit/Transmission)
- Crate workspace layout
- Coding standards and unsafe policy

## Track 2 — Phase 1: Foundation crates

Build and fuzz-test:
- `rt-bencode` — parser + canonical encoder, property-tested
- `rt-metainfo` — .torrent and magnet parsing, path sanitization, infohash v1/v2/hybrid
- `rt-hash` — SHA-1 / SHA-256 piece verification, bounded hashing pool
- `rt-piece-map` — piece-to-file mapping, request boundary math
- `rt-config` — TOML config, env override, validation
- `rt-testkit` — test fixtures, synthetic torrent generators

Exit criteria: parse valid torrents, reject malformed/malicious torrents, compute v1 infohash correctly, map pieces to files, fuzz parser, property-test bencode invariants.

## Track 2 — Phase 2: Storage and recheck engine

Build:
- File planner and path sanitizer
- Storage root abstraction (mount-aware)
- Piece verifier with bounded hashing pool
- Resumable, cancellable, restart-safe recheck jobs
- Per-mount disk scheduler (queue depth, HDD vs SSD profile, priority)
- Dry-run import mode

Exit criteria: verify existing complete torrent without downloading, detect missing/corrupt files, survive crash mid-check, resume recheck, dry-run import a 15k-torrent library.

## Track 2 — Phase 3: Tracker engine

Build:
- HTTP and UDP announce
- Compact peer parsing
- Tracker tiers, retry, and backoff
- Scrape
- Announce accounting (uploaded/downloaded/left, events)
- Private tracker mode (disable DHT/PEX/LSD)
- Restart jitter — no announce storms

Exit criteria: started/completed/stopped events correct, interval respected, tracker failures classified, restart does not announce-storm, private torrent disables DHT unless overridden.

## Track 2 — Phase 4: TCP seeding MVP

Build:
- TCP listener and handshake
- Bitfield/have-all
- Interested/choke/unchoke
- Request validation and piece serving
- Upload accounting per torrent/tracker/session
- Per-torrent and global peer caps

Exit criteria: seed a complete torrent to another client, seed multi-file torrent, reject invalid requests, maintain correct upload stats, run 1k passive seeding torrents.

## Track 2 — Phase 5: Session daemon

Build:
- `rusttorrentd` binary
- SQLite session DB with migrations
- Torrent lifecycle supervisor
- Append-only event log
- Job queue
- Health and metrics endpoints
- Clean shutdown with stopped announces

Exit criteria: add torrent, import complete torrent, start/stop/pause, restart cleanly, crash-recover, expose Prometheus metrics.

## Track 2 — Phase 6: qBittorrent API compatibility v1

Priority 1 endpoints:
- auth, app/version, app/webapiVersion
- torrents/info, add, pause, resume, delete, recheck, reannounce
- torrents/files, trackers, setCategory, addTags
- sync/maindata (delta semantics)
- transfer/info

Exit criteria: Prowlarr/Sonarr/Radarr/autobrr can add torrents; qBit-compatible clients can list/pause/resume/delete/recheck/reannounce; sync/maindata works.

## Track 2 — Phase 7: Downloading

Build:
- Rarest-first piece picker
- Request scheduler and endgame mode
- Piece verification on receive
- File priority
- Partial download resume
- Magnet metadata exchange

Exit criteria: download Linux ISO from public swarm, resume partial download, handle corrupt piece, complete and transition to seeding.

## Track 2 — Phase 8: Scale hardening

Target: 10k → 15k torrents, 200+ TB simulation, tracker jitter, low idle CPU, bounded memory.

Exit criteria: 15k torrents cold start under target, API responsive, recheck does not starve seeding, tracker manager avoids burst failures, UI cache consistent.

## Track 2 — Phase 9: Web UI

Full UI replacing the Track 1 WebUI, backed by the native engine API. Same design principles: virtualized table, server-side filter/sort, delta sync, bulk dry-run previews, "why is this not seeding?" diagnostic path.

## Track 2 — Phase 10: DHT / PEX / LSD / uTP

Implemented as native protocol/policy surface. Private-tracker profiles keep
DHT/PEX/LSD disabled by default; public-swarm certification remains the release
quality bar.

## Track 2 — Phase 11: BEP 52 / v2 / hybrid torrents

Implemented for v2/hybrid parsing, file trees, SHA-256 file-root verification,
hybrid torrent identity, durable metadata projection, pure-v2 magnet
placeholders, fast-resume identity, and qBit/Transmission/Deluge-compatible API
surfaces.

## Track 2 — Phase 12: Production 1.0

Required before 1.0:
- [x] Migration tools from rTorrent, qBittorrent, Transmission
- [x] qBit API compatibility report
- [x] Public benchmark report via native scale certification
- [x] Threat model review
- [x] Backup/restore docs
- [x] Native deployment docs
- [x] Prometheus metrics endpoint and metrics certification
- [x] Disaster recovery guide
- [x] Native packaging examples beyond source builds: systemd unit, Docker image, Compose, Kubernetes example
- [x] Prometheus/Grafana dashboard artifact
- [x] Arch/AUR package template

## Track 2 benchmark targets

### Engine

| Scenario | Target |
|---|---|
| Cold start — 15k torrents | < 120s |
| Steady idle RAM — 15k torrents | < 2.5 GB |
| Crash recovery — 15k torrents | < 30s |
| Session restore — no global recheck required | ✓ |
| Tracker announce storm after restart | 0 |
| Recheck throughput — NVMe | measure and publish |
| Recheck throughput — HDD | measure and publish |

### API

| Scenario | Target |
|---|---|
| `/api/v2/torrents/info` — 15k torrents | < 250ms |
| `/api/v2/sync/maindata` delta | < 50ms |
| Native filter/sort — 15k | < 250ms |
| Bulk tag — 10k torrents | < 2s |

### UI

| Scenario | Target |
|---|---|
| Initial load — 15k torrents | < 3s |
| Filter response | < 200ms |
| Torrent detail open | < 100ms |
| Bulk preview — 10k | < 1s |

## Track 2 — "best in class" acceptance criteria

These criteria now map to concrete tests, docs, or certification gates instead
of being tracked as loose roadmap wishes. The native rewrite is not blocked on
the Track 1 sidecar for engine state; remaining 1.0 work is packaging and
operator-facing polish.

| Criterion | Status | Evidence |
|---|---|---|
| 15k torrents loaded and manageable | Done | `rt-metrics` scale tests and `scripts/native_engine_certification_report.sh` |
| 200+ TB library imported without forced global recheck | Done | `rt-migrate` dry-run/import planning and durable DB import tests |
| qBit-compatible API works with Sonarr/Radarr/Prowlarr/autobrr | Done | Track 1 live certification plus native qBit projection tests |
| Cold restart does not announce-storm trackers | Done | tracker restart storm scale test |
| Rechecks are queued, resumable, cancellable, and visible | Done | durable job queue, recheck job, and engine recovery tests |
| Bulk path/category/tracker edits have dry-run previews | Done | native bulk preview and storage planning tests |
| Storage engine has per-mount queueing and backpressure | Done | `rt-storage` scheduler and starvation tests |
| UI can filter/sort 15k torrents without browser death | Done | virtualized WebUI and native/API scale targets |
| Crash during move/check/import is recoverable | Done | job recovery, move planning, and migration atomicity tests |
| Private tracker mode disables DHT/PEX/LSD unless explicitly enabled | Done | tracker policy tests |
| Metrics and event logs explain failures without log spelunking | Done | native metrics, diagnostics, and append-only event log |
| Public benchmark report published | Done | native certification report output under `certification/reports/` |
