# Architecture

## Overview

rtorrentNG is a four-layer stack built around rTorrent as the torrent engine. The layers are cleanly separated so each can be replaced or improved independently.

```
┌─────────────────────────────────────────────────────────┐
│                      External Tools                      │
│   Prowlarr · Sonarr · Radarr · autobrr · cross-seed     │
│              NZB360 · Transdrone · etc.                  │
└────────────────────────┬────────────────────────────────┘
                         │ qBittorrent-compatible API
┌────────────────────────▼────────────────────────────────┐
│                    WebUI (React/Vite)                    │
│   Virtualized torrent table · Bulk ops · Tracker views  │
│              Storage dashboard · Event log               │
└────────────────────────┬────────────────────────────────┘
                         │ Native REST + WebSocket
┌────────────────────────▼────────────────────────────────┐
│                  Sidecar Daemon (Go)                     │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐  │
│  │ Native   │  │ qBit     │  │ WebSocket│  │ Cache  │  │
│  │ REST API │  │ Compat   │  │ Events   │  │ SQLite │  │
│  └──────────┘  └──────────┘  └──────────┘  └────────┘  │
│                    Auth · Tokens · CSRF                  │
└────────────────────────┬────────────────────────────────┘
                         │ Trusted XMLRPC over local SCGI socket
┌────────────────────────▼────────────────────────────────┐
│                  rTorrent Engine                         │
│         Pinned version · tinyxml2 build                  │
│         Tuned session/announce/recheck config            │
└─────────────────────────────────────────────────────────┘
```

## Layer 1: Engine Profile

**Location:** `engine-profile/`

rTorrent is the BitTorrent engine. We do not modify rTorrent's core behavior significantly — we pin a known-good version, standardize the build, and enforce a hardened runtime configuration.

### Engine choices

| Decision | Choice | Rationale |
|---|---|---|
| rTorrent version | 0.16.11 (pinned) | Current stable; pre-release descriptor removed |
| XMLRPC backend | tinyxml2 | xmlrpc-c causes erratic RPC behavior (rakshasa/rtorrent#1636) |
| SCGI exposure | Local socket only | Never expose SCGI to network; sidecar is the API firewall |
| XMLRPC trust model | Sidecar is the only trusted client | Sidecar connects via trusted socket path; no untrusted passthrough |

### Key configuration areas

- Session directory and resume behavior
- Announce throttling (avoid tracker bans on restart)
- Recheck concurrency limits (avoid disk storm)
- Mount/path policies
- Memory/mmap tuning
- systemd socket activation or manual socket path

## Layer 2: Sidecar Daemon

**Location:** `sidecar/`
**Language:** Rust
**Binary:** `rtorrentng`

The sidecar is the control plane. It is the only process that talks to rTorrent XMLRPC. Everything else — browser, automation tools, scripts — talks to the sidecar.

### Responsibilities

- Maintain a live torrent state cache (SQLite) synced from rTorrent
- Serve the native REST API
- Serve the qBittorrent-compatible API
- Serve WebSocket event stream with delta diffs
- Handle auth (session tokens, API tokens, OIDC proxy header trust)
- Manage long-running bulk operations (bulk move, bulk tracker edit, recheck queue)
- Expose Prometheus metrics and health endpoint
- Structured JSON logs via `log/slog`

### Package layout

```
sidecar/
  Cargo.toml
  src/
    main.rs           — startup, signal handling, wires all modules
    config.rs         — TOML config, env override (RTNG_*), validation
    sync.rs           — background tokio task: poll rTorrent → upsert cache → broadcast WS events
    rtorrent/
      mod.rs
      client.rs       — async XMLRPC/SCGI client (Unix socket or TCP)
      torrents.rs     — d.multicall2 query, CRUD, set_user_agent / get_user_agent
    api/
      mod.rs
      server.rs       — axum Router, AppState
      handlers.rs     — native REST handlers (torrents, user-agent settings, health)
      ws.rs           — WebSocket upgrade, Event enum, broadcast fan-out
    qbcompat/
      mod.rs
      handlers.rs     — qBittorrent v2 API shim (auth, app, torrents, sync, transfer)
    cache/
      mod.rs
      db.rs           — rusqlite schema, WAL, upsert/delete, migrations
      query.rs        — server-side filter/sort/paginate (no ORM)
```

### State sync model

```
rTorrent ──XMLRPC poll──► sidecar cache (SQLite)
                                │
                    ┌───────────┤
                    │           │
              REST clients   WebSocket
              (on-demand     (push delta
               query)         on change)
```

Poll interval: configurable, default 2s. Delta detection: compare hash of torrent state fields; push WS event only on change. This avoids full-refresh spam in the browser.

### qBittorrent compatibility shim

The qBit shim translates qBittorrent API calls to internal sidecar operations. It does not call rTorrent directly. This means qBit API calls benefit from the same caching, auth, and safety guarantees as native API calls.

Compatibility target: qBittorrent Web API v2 (as documented for qBittorrent 5.x).

## Layer 3: WebUI

**Location:** `webui/`
**Stack:** React 19, TypeScript strict, Vite, TanStack Query, TanStack Virtual, TanStack Table

### Key design constraints

1. **Virtualized table** — only DOM rows in the viewport are rendered. 100k-row list must be smooth.
2. **Server-side sort/filter** — the browser never holds the full torrent list. Queries go to sidecar with `sort`, `filter`, `offset`, `limit` params.
3. **Delta sync** — WebSocket connection receives push events for torrent state changes. No polling loops.
4. **No right-click dependency** — all actions available in side panels and toolbars. Mobile-safe.
5. **Saved views** — named filter+sort+column presets stored in sidecar, synced across sessions.

### View structure

```
App
├── TorrentList (virtualized, server-side data)
│   ├── FilterBar (saved views, quick filters, search)
│   ├── TorrentTable (TanStack Virtual rows)
│   └── BulkActionBar (preview before execute)
├── TorrentDetail (sidebar/drawer)
│   ├── General, Files, Trackers, Peers, Speed
│   └── Actions panel
├── StorageDashboard
├── TrackerHealth
├── RatioGroups
├── EventLog
└── Settings
```

## Layer 4: Deployment

**Location:** `deploy/`

### Targets

- **Docker:** single container (rTorrent + sidecar + static WebUI assets)
- **Compose:** rTorrent container + sidecar container + nginx reverse proxy
- **systemd:** two units — `rtorrent.service` and `rtorrentng-sidecar.service`
- **nginx:** example config with WebSocket proxy, static asset serving, auth header forwarding
- **Helm:** minimal chart for Kubernetes/homelab deployments

### Container strategy

```
┌─────────────────────────────────────┐
│           nginx (reverse proxy)     │
│  /        → webui static assets     │
│  /api/    → sidecar :8080           │
│  /ws      → sidecar :8080 (upgrade) │
└─────────────────────────────────────┘
┌─────────────────┐  ┌─────────────────┐
│ sidecar :8080   │  │ rTorrent        │
│ (rtorrentng)    │◄─│ SCGI socket     │
└─────────────────┘  └─────────────────┘
          shared volume: /run/rtorrent/rpc.sock
          shared volume: /data (downloads)
          shared volume: /session (rTorrent session)
```
