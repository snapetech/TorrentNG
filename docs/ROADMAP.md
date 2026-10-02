# Roadmap

This project has one WebUI/API product with two backend arrangements. The
compatible-client integration delivered immediate value on top of rTorrent and
remains available for existing-client deployments, migration, and comparison.
The TorrentNG client (`torrentngd`) is the first-party Rust transfer client for
deployments where TorrentNG owns storage, persistence, and protocol behavior.
The Track 1/Track 2 labels below are historical development labels.

---

# Historical Track 1 — rTorrent Compatible-Client Integration

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

## Phase 2 — Compatible-client service MVP

The Rust `torrentng` service becomes the WebUI/API control plane. ruTorrent can
still coexist.

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
1. [x] Virtualized torrent table with bounded row rendering
2. [x] Server-side filter + sort
3. [x] WebSocket delta sync
4. [x] Bulk ops with dry-run preview
5. [x] Tracker health view
6. [x] Ratio group management
7. [x] Storage/mount dashboard
8. [x] Saved views
9. [x] Mobile-safe interactions (no right-click required)

## Phase 5 — Workflow platform

Compatible-client-service-managed replacement for high-value ruTorrent plugins.

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

## Track 1 historical diagnostics (non-release)

The following targets are retained as historical context only. Numeric
torrent-count capacity proofs are not current release gates.

| Scenario | Target |
|---|---|
| Representative fixture — UI first paint | measure and publish |
| Representative fixture — `/torrents/info` API | measure and publish |
| `/sync/maindata` delta under normal churn | < 50ms |
| Compatible-client service memory under a bounded fixture | measure and publish |
| Cold start + first torrent list ready | < 5s |

---

# Historical Track 2 — TorrentNG Rust Client

A ground-up Rust BitTorrent client daemon for large libraries, private-tracker
seeding, and operational observability. This track is implemented across the
workspace; local certification scripts exist, but numeric capacity and
production-scale/public certification are not current release claims.

See `docs/ENGINE.md` for the full design.

## North Star

> A headless-first BitTorrent WebUI/API product with a first-party,
> next-generation TorrentNG client and compatible-client integrations that
> can move into, out of, and alongside the major torrent client ecosystems:
> qBittorrent, Transmission, Deluge, rTorrent, uTorrent/BitTorrent Classic,
> BiglyBT/Vuze, Tixati, common automation tools, and real BitTorrent swarms.

The engine is a **massive-library seeding engine** and a **compatibility-first
torrent control plane**. It should be able to import existing state, project the
APIs tools expect, interoperate on the wire, and expose a TorrentNG-client model
that is more observable and easier to operate than the historical client-specific
internals it replaces. TorrentNG-client downloading, DHT/uTP protocol crates, and BEP 52
metadata/storage/API support are part of the rewrite surface; streaming remains
outside the first production target.

## Track 2 — Phase 0: Research and design lock

Deliverables:
- BEP compliance matrix
- API compatibility matrix (qBit, Transmission)
- Storage design doc and invariants
- Session DB schema
- Threat model
- Benchmark plan with host-selected bounded synthetic diagnostics
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

Exit criteria: verify existing complete torrent without downloading, detect missing/corrupt files, survive crash mid-check, resume recheck, and dry-run import a representative library.

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

Exit criteria: seed a complete torrent to another client, seed multi-file torrent, reject invalid requests, maintain correct upload stats, and exercise a bounded passive-seeding fixture.

## Track 2 — Phase 5: Session daemon

Build:
- `torrentngd` binary
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

## Track 2 — Phase 8: Runtime/resource hardening

Target: large-library resource behavior, tracker jitter, low idle CPU, bounded memory, and storage backpressure.

Exit criteria: representative fixture cold start is measured, API remains responsive, recheck does not starve seeding, tracker manager avoids burst failures, and UI cache stays consistent.

### Track 2 — cross-phase: crash safety

Completed: durability barrier before resume records (Phase 2), plus the crash-safety
work in [CRASH_SAFETY.md](CRASH_SAFETY.md): completion gate, `fast`-mode
honesty, `fallocate(2)` preallocation (no zero emulation), run marker with host
crash detection, recovery policy (`watermark`/`recent`/`full`), allocation audit,
mount durability ratings, optional read-back verification, integrity-regression
events, migration crash detection for rTorrent, a runtime settings API and WebUI
panel, and a power-cut model test. Also done: macOS/FreeBSD/Windows probes,
pure-v2 read-back verification and events, an explicit `finalizing` flag, and
qBittorrent/Deluge/`--source-lock` migration crash detection, and per-location
policies (`[[crash_safety.path_policies]]`) so mixed storage can differ. Open: a run on
real Windows, macOS and FreeBSD hardware (Windows ran under wine, the others
were only type-checked); the Windows allocated-range success path; NTFS
valid-data-length detection; page-cache drop on macOS and Windows; verifying the
qBittorrent/Deluge lock locations against the real clients.

## Track 2 — Phase 9: Web UI

Full UI shared with the compatible-client integration, backed by the TorrentNG
client API. Same design principles: virtualized table, server-side filter/sort,
delta sync, bulk dry-run previews, "why is this not seeding?" diagnostic path.

## Track 2 — Phase 10: DHT / PEX / LSD / uTP

Implemented for DHT policy, DHT peer discovery plumbing, PEX policy/parsing, and
uTP packet-codec/state plus async UDP transport primitives, incoming/outgoing
engine peer-wire paths, and uTP metadata fetch. The runtime capability reports
uTP transport when policy leaves at least one uTP path enabled. Private-tracker
profiles keep DHT/PEX/LSD disabled by default; public-swarm certification
remains the release quality bar.

## Track 2 — Phase 11: BEP 52 / v2 / hybrid torrents

Implemented for v2/hybrid parsing, file trees, SHA-256 file-root verification,
hybrid torrent identity, durable metadata projection, pure-v2 `btmh` metadata
completion, BEP 52 piece-layer exchange/proof validation, fast-resume identity,
and qBit/Transmission/Deluge-compatible API surfaces. Complete pure-v2
metainfo and verified pure-v2 magnet metadata promote through the same native
v2 runtime; public-client, target-device, and long-duration interoperability
remain qualification evidence rather than unimplemented protocol work.

## Track 2 — Phase 12: Production 1.0

Required before 1.0:
- [x] Migration tools from rTorrent, qBittorrent, Transmission
- [x] qBit API compatibility report
- [x] TorrentNG-client deterministic resource-regression diagnostics are
  checked in. Numeric torrent-count capacity proof is intentionally outside the
  release checklist.
- [x] Threat model review
- [x] Backup/restore docs
- [x] TorrentNG client deployment docs
- [x] Prometheus metrics endpoint and metrics certification
- [x] Disaster recovery guide
- [x] TorrentNG-client packaging examples beyond source builds: systemd unit, Docker image, Compose, Kubernetes example
- [x] Prometheus/Grafana dashboard artifact
- [x] Arch/AUR package template

## Track 2 diagnostic targets (non-release)

### Engine

| Scenario | Target |
|---|---|
| Cold start — representative fixture | measure and publish |
| Steady idle RAM — bounded fixture | measure and publish |
| Crash recovery — representative fixture | measure and publish |
| Session restore — no global recheck required | ✓ |
| Tracker announce storm after restart | 0 |
| Recheck throughput — NVMe | measure and publish |
| Recheck throughput — HDD | measure and publish |

### API

| Scenario | Target |
|---|---|
| `/api/v2/torrents/info` — representative fixture | measure and publish |
| `/api/v2/sync/maindata` delta | < 50ms |
| TorrentNG-client filter/sort — representative fixture | measure and publish |
| Bulk tag — bounded fixture | measure and publish |

### UI

| Scenario | Target |
|---|---|
| Initial load — representative fixture | measure and publish |
| Filter response | < 200ms |
| Torrent detail open | < 100ms |
| Bulk preview — bounded fixture | measure and publish |

## Track 2 — "best in class" acceptance criteria

These criteria now map to concrete tests, docs, or certification gates instead
of being tracked as loose roadmap wishes. The TorrentNG client is not blocked on
the compatible-client service for client state; remaining 1.0 work is packaging and
operator-facing polish.

| Criterion | Status | Evidence |
|---|---|---|
| Large-library rows remain manageable under bounded rendering | Done | `rt-metrics` regression tests and virtualized WebUI |
| 200+ TB library imported without forced global recheck | Done | `rt-migrate` dry-run/import planning and durable DB import tests |
| qBit-compatible API works with Sonarr/Radarr/Prowlarr/autobrr | Done | Compatible-client live certification plus TorrentNG-client qBit projection tests |
| Cold restart does not announce-storm trackers | Done | tracker restart storm scale test |
| Rechecks are queued, resumable, cancellable, and visible | Done | durable job queue, recheck job, and engine recovery tests |
| Bulk path/category/tracker edits have dry-run previews | Done | TorrentNG-client bulk preview and storage planning tests |
| Storage engine has per-mount queueing and backpressure | Done | `rt-storage` scheduler and starvation tests |
| UI can filter/sort without unbounded browser rendering | Done | virtualized WebUI and TorrentNG API regression tests |
| Crash during move/check/import is recoverable | Done | job recovery, move planning, and migration atomicity tests |
| Private tracker mode disables DHT/PEX/LSD unless explicitly enabled | Done | tracker policy tests |
| Metrics and event logs explain failures without log spelunking | Done | TorrentNG metrics, diagnostics, and append-only event log |
| Deterministic performance diagnostic available | Done | Optional benchmark output under `benchmarks/` |
