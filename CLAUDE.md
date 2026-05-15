# rtorrentNG — Claude Context

## What this project is

**rtorrentNG** is a modern distribution/fork bundle targeting headless power-user seeding at scale (10k–100k torrents, 200+ TB).

It is NOT a ruTorrent cosmetic fork. It is NOT a new BitTorrent engine.

It is four layers built around rTorrent as the engine:

1. **engine-profile/** — Pinned rTorrent build config, SCGI/socket setup, tuning profiles
2. **sidecar/** — Go daemon: trusted local rTorrent RPC in, sane REST/WebSocket/qBit-compat API out
3. **webui/** — React+Vite frontend, virtualized table, talks only to sidecar API
4. **deploy/** — Docker, Compose, systemd, nginx/Caddy, Helm stubs

## Core architectural decisions

- External tools (Prowlarr, Sonarr, Radarr, autobrr, cross-seed) talk to the **sidecar** via qBittorrent-compatible API
- The browser talks to the **sidecar** via native REST + WebSocket events
- **Nothing** talks to rTorrent XMLRPC/SCGI directly except the sidecar
- The sidecar runs on localhost beside rTorrent, communicates over trusted local SCGI socket
- Auth, tokens, OIDC, CSRF protection all live in the sidecar layer

## Why rTorrent as engine

rTorrent/libTorrent is the strongest baseline for large headless seed libraries:
- Low memory growth over time
- Strong session persistence and resume
- Low churn seeding workload fits its concurrency model
- libtorrent (rTorrent's, not rasterbar's) has better mmap/memory behavior at scale than libtorrent-rasterbar 2.x

## Key problem rTorrent/ruTorrent has today

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
- Server-side sort/filter via sidecar API — never load all torrents to browser
- No right-click dependency for mobile support
- Delta sync via WebSocket — no full-refresh polling loops
- Settings view includes `UserAgentPanel` component for live user-agent management

## qBittorrent API compatibility targets (Phase 1)

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
- 50k synthetic: sidecar API `/torrents/info` < 500ms
- `/sync/maindata` delta < 50ms under normal churn
- sidecar memory < 500MB at 15k torrents after 24h

## Two-track strategy

**Track 1 — rTorrent sidecar** (current): fix rTorrent/ruTorrent pain without replacing the engine. Phases 0–5.

**Track 2 — Native Rust engine** (after Track 1 ships): ground-up Rust BitTorrent daemon, 10k–100k torrents, 200+ TB, seeding-first. 12 phases. See `docs/ENGINE.md` for full design.

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

- Rust sidecar: axum + tokio; no unsafe except in deps; anyhow for errors in binary, thiserror for library errors
- WebUI: TypeScript strict, TanStack Query for server state, TanStack Virtual for table
- No ORM; raw SQL via rusqlite with bundled SQLite (no system dep)
- Config file: `~/.config/rtorrentng/config.toml` or `/etc/rtorrentng/config.toml`; env vars `RTNG_*` override all
- All API responses: JSON, snake_case keys
- Logs: structured JSON via tracing + tracing-subscriber JSON layer

## user_agent

Configurable via `[rtorrent] user_agent` in config or `RTNG_USER_AGENT` env var.
Default: `rtorrentNG/0.1.0 libtorrent/0.16.11` (used in packaged releases).
Pushed to rTorrent via `network.http.user_agent.set` on startup.
Runtime update: `PUT /api/v1/settings/user-agent` or Settings panel in WebUI.
See `docs/CONFIGURATION.md` for known values.
Do NOT document the reason for this feature in user-facing docs.
