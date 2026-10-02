# TorrentNG — Claude Context

## What this project is

**TorrentNG** is a modern torrent management stack targeting headless
power-user seeding at scale (10k-100k+ torrents, 200-500+ TB).

It is NOT a ruTorrent cosmetic fork. TorrentNG is the shared WebUI/API product:
it can sit in front of an existing compatible torrent client or run its own
first-party TorrentNG client, `torrentngd`.

It has two backend arrangements:

1. **TorrentNG client** — `crates/torrentngd` owns torrent state, peer traffic,
   tracker state, storage, rechecks, jobs, metrics, REST/SSE, and compatibility
   API projections.
2. **Compatible-client integration** — `sidecar/torrentng` hosts the shared
   WebUI/API and connects it to rTorrent, qBittorrent, Transmission, Deluge,
   or a separate `torrentngd`; the selected client owns transfer and session
   state.

Important layers:

1. **engine-profile/** — Pinned rTorrent build config, SCGI/socket setup, tuning profiles
2. **sidecar/** — Rust compatible-client WebUI/API service (path retained for compatibility)
3. **crates/** — TorrentNG client crates and `torrentngd`
4. **webui/** — React+Vite frontend, virtualized table, talks to TorrentNG or compatible-client APIs
5. **deploy/** — Docker, Compose, systemd, nginx, Kubernetes examples

## Core architectural decisions

- External tools (Prowlarr, Sonarr, Radarr, autobrr, cross-seed) talk to TorrentNG through compatibility APIs, primarily the qBittorrent-compatible API.
- The browser talks to TorrentNG REST/SSE/WebSocket-facing APIs, never directly to rTorrent SCGI.
- When `torrentngd` serves the product directly, it is the source of truth and does not require rTorrent, XMLRPC, or the integration service.
- In a compatible-client integration, **nothing** talks to rTorrent XMLRPC/SCGI directly except the `torrentng` service.
- The compatible-client service may run beside rTorrent or connect to a remote supported client; it remains a WebUI/API, cache, and migration/facade layer.
- Auth, tokens, CSRF/OIDC/reverse-proxy trust policy live in the TorrentNG API layer.

## Why the TorrentNG client exists

The compatible-client integration fixes immediate rTorrent/ruTorrent control
plane pain, but it cannot replace the selected client's transfer engine or fix
engine-level limits:

- rTorrent owns storage behavior and has no TorrentNG userspace disk scheduler.
- Rechecks are not durable TorrentNG jobs with pause/resume/cancel semantics.
- Torrent lifecycle history is limited compared with TorrentNG structured events.
- The `torrentng` service must poll and translate XMLRPC state.
- Engine behavior depends on rTorrent/libtorrent build details.
- BEP 52/v2, compatibility facades, migration, and metrics are simpler when
  projected from one TorrentNG-client model.

See `docs/ENGINE_REWRITE.md` for the practical guide and `docs/ENGINE.md` for
the deeper design.

## Why rTorrent still exists

rTorrent/libTorrent remains a strong baseline for large headless seed libraries:
- Low memory growth over time
- Strong session persistence and resume
- Low churn seeding workload fits its concurrency model
- Existing user deployments need migration and comparison paths

## Key problem rTorrent/ruTorrent had in Track 1

- rTorrent 0.16.9+ introduced trusted/untrusted XMLRPC connection model
- Raw SCGI/httprpc passthrough breaks external clients (`load.start` blocked for untrusted connections)
- Prowlarr, Sonarr, Radarr, Transdrone, NZB360 all hit this
- xmlrpc-c build path is erratic; tinyxml2 preferred
- ruTorrent 10k+ torrent UI is sluggish (hotfix shipped in v5.2.10)
- No clean daemon/API/event model — everything is PHP polling XMLRPC

## torrentngd CLI subcommands

`crates/torrentngd` is daemon-first, but `argv[1]` dispatches two offline
state tools before the daemon starts (run via `spawn_blocking`):

- `torrentngd migrate --source <client> --from <dir> [--apply]` — import other
  clients' state into the TorrentNG client model. Dry-run by default; `--apply` writes
  DB rows + `rt-fastresume` state and persists `.torrent` blobs into
  `session_dir/torrents`. `--remap OLD=NEW`, `--policy verify|trust-hints|trust-all`.
- `torrentngd export --format <client> --to <dir> [--apply]` — reverse
  migration (anti-lock-in). Reads TorrentNG-client state read-only, writes the target
  client layout.

Both reuse `crates/rt-migrate` (`rt_migrate::export` for the reverse path) and
report fidelity buckets. Source code: `crates/torrentngd/src/{migrate,export}.rs`.
Certification: `crates/rt-migrate/tests/round_trip_matrix.rs` (all clients ×
import/export/round-trip) plus `scripts/migration_corpus_certification.sh`.

## Crash safety (torrentngd)

Full guide: `docs/CRASH_SAFETY.md`. The invariant to preserve in any storage,
fastresume or completion change: **never persist a claim (resume record,
`completed_at`, `Seeding`, tracker `completed`) before the data it describes is
durable.**

- Fastresume is written only after the payload barrier (`save_fastresume_with` in
  `crates/rt-engine/src/torrent_task.rs`); a record saved without a data sync is
  `durability.synced = false` and is discarded after a host crash.
- A finished download is held (`CompletionGate`, reported as `amount_left = 1`)
  until a forced barrier succeeds; do not add code paths that reach `Seeding`
  around it. Pure-v2 tasks have their own barrier in `torrent_task_v2.rs`.
- Never preallocate by writing zeros (`posix_fallocate` emulates that); use
  native `fallocate` or a sparse length so unwritten space stays visible to the
  allocation audit (`rt-storage::alloc_audit`).
- Host crash vs process crash is decided from `run_marker.json` plus the OS boot
  identity (`rt-fastresume::run_marker`, `rt-storage::boot`); the pure recovery
  decision is `rt-fastresume::recovery::decide_recovery`.
- Settings live under `[crash_safety]`, are runtime-editable
  (`/api/v1/settings/crash-safety`, WebUI Settings → Backend), and unknown keys are
  rejected. OS-specific probes must degrade to "unsupported", never fail startup.
- Per-torrent decisions (gate, recovery, audit, read-back) must go through
  `CrashSafetyRuntime::for_location(save_root)` so `[[crash_safety.path_policies]]`
  applies; reading the global fields directly bypasses per-location overrides.
- The power-cut model tests (`power_cut_*` in `torrent_task.rs`) must keep failing
  for the unprotected configuration; that is what proves the harness has teeth.

## torrentng — compatible-client WebUI/API service

**Entry:** `sidecar/src/main.rs` (the source path is retained for compatibility)
**Crates:** axum 0.7, tokio, serde/serde_json, toml, rusqlite (bundled), tracing, anyhow, quick-xml
**Modules:**
- `config` — TOML config loading, env override (`TNG_*`)
- `rtorrent::client` — async XMLRPC/SCGI client over Unix socket or TCP
- `rtorrent::torrents` — `d.multicall2` torrent query, CRUD ops, `set_user_agent`
- `api::server` — axum router, AppState
- `api::handlers` — TorrentNG REST handlers including `GET/PUT /api/v1/settings/user-agent`
- `api::ws` — WebSocket event broadcast
- `qbcompat` — qBittorrent v2 API shim
- `cache::db` — rusqlite schema, upsert/delete, WAL mode
- `cache::query` — server-side filter/sort/paginate
- `sync` — background tokio task: selected-client poll → cache upsert → WS broadcast

**API surface:**
- `/api/v1/...` — TorrentNG JSON API
- `/api/v1/settings/user-agent` — GET/PUT user-agent (live, pushes to rTorrent)
- `/api/qb/v2/...` — qBittorrent-compatible passthrough
- `/ws` — WebSocket event stream
- `/health` — health check

## webui — React+Vite

**Entry:** `webui/src/main.tsx`
**Key constraints:**
- Virtualized torrent table (TanStack Virtual or similar) — keep rendering and
  client-side state bounded
- Server-side sort/filter via TorrentNG or compatible-client API — never load all torrents to browser
- No right-click dependency for mobile support
- Delta sync via WebSocket — no full-refresh polling loops
- Settings view includes `UserAgentPanel` component for live user-agent management
- Settings → Backend also includes `CrashSafetyPanel` (native client only, gated
  on the `supports_crash_safety` capability); torrents held by the completion gate
  show as **Finalizing** (`isFinalizing` in `api/client.ts`)

## qBittorrent API compatibility targets

Must pass *arr/autobrr integration tests:
- `POST /api/qb/v2/auth/login`
- `GET  /api/qb/v2/app/version`
- `GET  /api/qb/v2/app/webapiVersion`
- `GET  /api/qb/v2/torrents/info`
- `POST /api/qb/v2/torrents/add`
- `POST /api/qb/v2/torrents/pause`
- `POST /api/qb/v2/torrents/resume`
- `POST /api/qb/v2/torrents/delete`
- `POST /api/qb/v2/torrents/recheck`
- `POST /api/qb/v2/torrents/reannounce`
- `GET  /api/qb/v2/torrents/trackers`
- `POST /api/qb/v2/torrents/editTracker`
- `GET  /api/qb/v2/torrents/files`
- `POST /api/qb/v2/torrents/filePrio`
- `POST /api/qb/v2/torrents/setCategory`
- `POST /api/qb/v2/torrents/addTags`
- `GET  /api/qb/v2/sync/maindata`
- `GET  /api/qb/v2/transfer/info`

## Performance diagnostics (non-release)

The optional checks in `benchmarks/` exercise API latency, delta sync, and
resource regressions with host-selected fixtures. They are diagnostic only;
numeric torrent-count capacity proofs and 24-hour stability are not current
release gates.

## Historical development tracks

**Track 1 — rTorrent compatible-client integration**: fix rTorrent/ruTorrent
control-plane pain without replacing the transfer client. Phases 0–5. The
historical track remains available as the rTorrent integration and comparison
path.

**Track 2 — TorrentNG client**: ground-up Rust BitTorrent client daemon,
10k-100k+ torrents, 200-500+ TB, seeding-first. This is the first-party owned
transfer path. See `docs/ENGINE_REWRITE.md` and `docs/ENGINE.md`.

### Track 1 phases

- **Phase 0:** Audit rTorrent 0.16.x + ruTorrent 5.3.x breakages
- **Phase 1:** Known-good distribution bundle (pinned versions, patched httprpc trust)
- **Phase 2:** Compatible-client service MVP (list/add/remove/start/stop/events)
- **Phase 3:** qBittorrent API compatibility shim
- **Phase 4:** Modern WebUI
- **Phase 5:** Plugin/workflow platform

### Track 2 phases (summary)

0. Research/design lock → 1. Foundation crates (bencode/metainfo/hash/piece-map) → 2. Storage + recheck client → 3. Tracker client → 4. TCP seeding MVP → 5. Session daemon → 6. qBit API compat v1 → 7. Downloading → 8. Runtime/resource hardening → 9. Web UI → 10. DHT/PEX/uTP → 11. BEP 52/v2 → 12. Production 1.0

## Conventions

- Rust services: axum + tokio; no unsafe except in deps; anyhow for errors in binaries, thiserror for library errors
- WebUI: TypeScript strict, TanStack Query for server state, TanStack Virtual for table
- No ORM; raw SQL via rusqlite with bundled SQLite (no system dep)
- TorrentNG-client config file: `TORRENTNGD_CONFIG`, `~/.config/torrentngd/config.toml`, or `/etc/torrentngd/config.toml`
- Compatible-client service config file: `~/.config/torrentng/config.toml` or `/etc/torrentng/config.toml`; env vars `TNG_*` override many service fields
- All API responses: JSON, snake_case keys
- Logs: structured JSON via tracing + tracing-subscriber JSON layer

## user_agent

Configurable via `[rtorrent] user_agent` in config or `TNG_USER_AGENT` env var.
Default: `rtorrent/0.16.11/0.16.11` (used in packaged releases).
Pushed to rTorrent via `network.http.user_agent.set` on startup.
Runtime update: `PUT /api/v1/settings/user-agent` or Settings panel in WebUI.
Peer ID family prefix: `-lt100B-`, fixed. The other 12 bytes are generated
and persisted per install (TorrentNG client: `<session_dir>/peer_id_suffix`;
compatible-client service: `<data_dir>/peer_id_suffix`) — NOT a shared literal.
A shared/hardcoded
peer_id suffix got a real user banned from a private tracker (MAM) for
"running multiple instances of the same client," because every install
without one presented the identical peer_id. Only set `[rtorrent] peer_id`
or `TNG_PEER_ID` to pin one specific install (e.g. a test fixture) — never
in a shared template, base image, or fleet-wide env var.
Do not strip the user-agent to `rtorrent/0.16.11`, do not use
`rtorrent/0.16.11/000` as a peer ID, and do not guess `-lt1011-`.
See `docs/TRACKER-IDENTITY.md` before changing this.
