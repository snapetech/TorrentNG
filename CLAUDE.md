# rtorrentNG — Claude Context

## What this project is

**rtorrentNG** is a modern torrent management stack targeting headless
power-user seeding at scale (10k–100k torrents, 200+ TB).

It is NOT a ruTorrent cosmetic fork. It now has a native Rust BitTorrent engine
rewrite as the primary runtime path, while the rTorrent-backed sidecar remains
available for migration, compatibility testing, and users who still want the
upstream rTorrent core.

It has two runtime tracks:

1. **Native rewrite** — `crates/rusttorrentd` owns torrent state, peer traffic,
   tracker state, storage, rechecks, jobs, metrics, native REST/SSE, and
   compatibility API projections.
2. **Track 1 rTorrent core** — rTorrent/libtorrent remains the BitTorrent
   engine; `sidecar/rtorrentng` bridges trusted local SCGI/XMLRPC into the
   WebUI, native REST facade, qBittorrent-compatible API, cache, auth, and
   metrics.

Important layers:

1. **engine-profile/** — Pinned rTorrent build config, SCGI/socket setup, tuning profiles
2. **sidecar/** — Rust daemon for rTorrent-backed deployments
3. **crates/** — Native engine crates and `rusttorrentd`
4. **webui/** — React+Vite frontend, virtualized table, talks to native/sidecar APIs
5. **deploy/** — Docker, Compose, systemd, nginx, Kubernetes examples

## Core architectural decisions

- External tools (Prowlarr, Sonarr, Radarr, autobrr, cross-seed) talk to rtorrentNG through compatibility APIs, primarily the qBittorrent-compatible API.
- The browser talks to native REST/SSE/WebSocket-facing APIs, never directly to rTorrent SCGI.
- In native mode, `rusttorrentd` is the source of truth and does not require rTorrent, XMLRPC, or the sidecar.
- In Track 1 sidecar mode, **nothing** talks to rTorrent XMLRPC/SCGI directly except the sidecar.
- The sidecar runs beside rTorrent, communicates over a trusted local SCGI socket, and remains a migration/facade layer.
- Auth, tokens, CSRF/OIDC/reverse-proxy trust policy live in the rtorrentNG API layer.

## Why the native rewrite exists

Track 1 fixed immediate rTorrent/ruTorrent pain, but could not fix engine-level
limits:

- rTorrent owns storage behavior and has no rtorrentNG userspace disk scheduler.
- Rechecks are not durable rtorrentNG jobs with pause/resume/cancel semantics.
- Torrent lifecycle history is limited compared with native structured events.
- The sidecar must poll and translate XMLRPC state.
- Engine behavior depends on rTorrent/libtorrent build details.
- BEP 52/v2, compatibility facades, migration, and metrics are simpler when
  projected from one native model.

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

## sidecar — Rust daemon

**Entry:** `sidecar/src/main.rs`
**Crates:** axum 0.7, tokio, serde/serde_json, toml, rusqlite (bundled), tracing, anyhow, quick-xml
**Modules:**
- `config` — TOML config loading, env override (`RTNG_*`)
- `rtorrent::client` — async XMLRPC/SCGI client over Unix socket or TCP
- `rtorrent::torrents` — `d.multicall2` torrent query, CRUD ops, `set_user_agent`
- `api::server` — axum router, AppState
- `api::handlers` — native REST handlers including `GET/PUT /api/v1/settings/user-agent`
- `api::ws` — WebSocket event broadcast
- `qbcompat` — qBittorrent v2 API shim
- `cache::db` — rusqlite schema, upsert/delete, WAL mode
- `cache::query` — server-side filter/sort/paginate
- `sync` — background tokio task: rTorrent poll → cache upsert → WS broadcast

**API surface:**
- `/api/v1/...` — native JSON API
- `/api/v1/settings/user-agent` — GET/PUT user-agent (live, pushes to rTorrent)
- `/api/qb/v2/...` — qBittorrent-compatible passthrough
- `/ws` — WebSocket event stream
- `/health` — health check

## webui — React+Vite

**Entry:** `webui/src/main.tsx`
**Key constraints:**
- Virtualized torrent table (TanStack Virtual or similar) — must handle 100k rows
- Server-side sort/filter via native or sidecar API — never load all torrents to browser
- No right-click dependency for mobile support
- Delta sync via WebSocket — no full-refresh polling loops
- Settings view includes `UserAgentPanel` component for live user-agent management

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

## Benchmark targets (in benchmarks/)

Every release must pass:
- 1k torrents: UI first paint < 1s, filter < 100ms
- 10k torrents: UI first paint < 2s, filter < 200ms
- 15k torrents: UI first paint < 3s, filter < 500ms
- 50k synthetic: compatibility API `/torrents/info` < 500ms
- `/sync/maindata` delta < 50ms under normal churn
- daemon/sidecar memory within release target at 15k torrents after 24h

## Two-track strategy

**Track 1 — rTorrent sidecar**: fix rTorrent/ruTorrent pain without replacing the engine. Phases 0–5. This remains available for migration and rTorrent-core comparison.

**Track 2 — Native Rust engine**: ground-up Rust BitTorrent daemon, 10k–100k torrents, 200+ TB, seeding-first. This is now the primary runtime path. See `docs/ENGINE_REWRITE.md` and `docs/ENGINE.md`.

### Track 1 phases

- **Phase 0:** Audit rTorrent 0.16.x + ruTorrent 5.3.x breakages
- **Phase 1:** Known-good distribution bundle (pinned versions, patched httprpc trust)
- **Phase 2:** Sidecar daemon MVP (list/add/remove/start/stop/events)
- **Phase 3:** qBittorrent API compatibility shim
- **Phase 4:** Modern WebUI
- **Phase 5:** Plugin/workflow platform

### Track 2 phases (summary)

0. Research/design lock → 1. Foundation crates (bencode/metainfo/hash/piece-map) → 2. Storage + recheck engine → 3. Tracker engine → 4. TCP seeding MVP → 5. Session daemon → 6. qBit API compat v1 → 7. Downloading → 8. Scale hardening (15k/200TB) → 9. Web UI → 10. DHT/PEX/uTP → 11. BEP 52/v2 → 12. Production 1.0

## Conventions

- Rust daemon/sidecar: axum + tokio; no unsafe except in deps; anyhow for errors in binary, thiserror for library errors
- WebUI: TypeScript strict, TanStack Query for server state, TanStack Virtual for table
- No ORM; raw SQL via rusqlite with bundled SQLite (no system dep)
- Native config file: `RUSTTORRENTD_CONFIG`, `~/.config/rusttorrentd/config.toml`, or `/etc/rusttorrentd/config.toml`
- Sidecar config file: `~/.config/rtorrentng/config.toml` or `/etc/rtorrentng/config.toml`; env vars `RTNG_*` override many sidecar fields
- All API responses: JSON, snake_case keys
- Logs: structured JSON via tracing + tracing-subscriber JSON layer

## user_agent

Configurable via `[rtorrent] user_agent` in config or `RTNG_USER_AGENT` env var.
Default: `rtorrent/0.16.11` (used in packaged releases).
Pushed to rTorrent via `network.http.user_agent.set` on startup.
Runtime update: `PUT /api/v1/settings/user-agent` or Settings panel in WebUI.
See `docs/CONFIGURATION.md` for known values.
Do NOT document the reason for this feature in user-facing docs.
