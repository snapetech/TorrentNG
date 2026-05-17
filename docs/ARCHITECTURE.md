# Architecture

## Overview

rtorrentNG now has two runtime modes:

- **Native engine mode:** `rusttorrentd` is the source of truth. It owns torrent
  state, SQLite session persistence, tracker state, peer wire tasks, storage,
  rechecks, jobs, metrics, native REST/SSE, and compatibility API projections.
- **Track 1 sidecar mode:** `sidecar/rtorrentng` remains available for existing
  rTorrent deployments. It talks to rTorrent over a trusted local SCGI socket,
  keeps a SQLite cache, and exposes the same WebUI and qBittorrent-compatible
  client surface while users migrate.

The native engine supersedes the wrapper/harness path for production native
mode. The wrapper remains useful as a migration bridge and rTorrent facade, not
as a required dependency of `rusttorrentd`.

For the practical engine-selection workflow, including how to swap between the
native rewrite and the rTorrent core for testing, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

```text
┌─────────────────────────────────────────────────────────┐
│                      External Tools                      │
│   Prowlarr · Sonarr · Radarr · autobrr · cross-seed     │
│              NZB360 · Transdrone · etc.                  │
└────────────────────────┬────────────────────────────────┘
                         │ qBit / Transmission / Deluge API
┌────────────────────────▼────────────────────────────────┐
│                    WebUI (React/Vite)                    │
│   Virtualized torrent table · Bulk ops · Tracker views  │
│              Storage dashboard · Event stream            │
└────────────────────────┬────────────────────────────────┘
                         │ Native REST + SSE
┌────────────────────────▼────────────────────────────────┐
│                rusttorrentd native daemon                │
│                                                          │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌─────────────┐ │
│  │ Native   │ │ Compat   │ │ Session  │ │ Jobs/events │ │
│  │ REST/SSE │ │ APIs     │ │ SQLite   │ │ metrics     │ │
│  └──────────┘ └──────────┘ └──────────┘ └─────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌─────────────┐ │
│  │ Tracker  │ │ Peer     │ │ Storage  │ │ Migration   │ │
│  │ manager  │ │ tasks    │ │ scheduler│ │ importers   │ │
│  └──────────┘ └──────────┘ └──────────┘ └─────────────┘ │
└─────────────────────────────────────────────────────────┘
```

Track 1 sidecar mode keeps this separate compatibility shape:

```text
WebUI / automation clients
          │
          ▼
sidecar/rtorrentng ── trusted XMLRPC over local SCGI ── rTorrent
          │
          └── SQLite cache, auth, qBit/native facade, metrics
```

## Native Engine

**Location:** `crates/`
**Binary:** `crates/rusttorrentd`

The native daemon wires the engine crates into one process. SQLite-backed
engine state is the source of truth for torrent rows, file metadata, trackers,
labels, jobs, metrics, and compatibility projections.

### Core Crates

- `rt-metainfo` parses v1, v2, hybrid torrents, and `btih`/`btmh` magnets.
- `rt-db` stores durable torrents, file rows, tracker rows, limits, jobs,
  events, settings, storage roots, mounts, tags, and categories.
- `rt-engine` supervises torrent tasks, metadata placeholders, rechecks,
  tracker state, DHT registration, shutdown, diagnostics, and restore.
- `rt-storage` provides root/mount abstractions, dry-run import/move/delete
  plans, scheduling, and v1/v2 verification.
- `rt-tracker`, `rt-peer-wire`, `rt-peer-manager`, `rt-piece-picker`,
  `rt-dht`, and `rt-utp` cover protocol behavior and peer/download mechanics.
- `rt-api-native`, `rt-api-qbit`, `rt-api-transmission`, and `rt-api-deluge`
  expose native and compatibility APIs over the same registry.
- `rt-migrate` imports rTorrent, qBittorrent, and Transmission state.
- `rt-metrics` and `rt-testkit` provide scale and certification evidence.

### Native Data Flow

```text
add torrent/magnet
      │
      ▼
parse metainfo or magnet identity
      │
      ▼
persist torrent row, metadata, labels, trackers, and event
      │
      ▼
spawn v1/hybrid torrent task or taskless pure-v2 metadata projection
      │
      ├── tracker manager persists announce/scrape state
      ├── peer tasks verify pieces before completion
      ├── storage scheduler throttles reads, writes, and rechecks
      └── APIs project registry + metadata + fastresume state
```

Startup restores persisted torrents from the DB and metadata store. Pure v2
rows restore as taskless metadata projections when there is no v1 peer-wire
task to spawn.

## Runtime API Layer

The API layer is intentionally a projection over engine state, not the internal
model:

- Native REST/SSE is snake_case and built for the WebUI and direct integrations.
- qBittorrent v2 compatibility preserves ecosystem behavior for automation.
- Transmission RPC supports session, torrent, tracker, file, queue, and magnet
  surfaces; v2 hashes project as BEP 52 `btmh` magnet links.
- Deluge RPC is a best-effort facade over the same registry and metadata.

`GET /health` is the runtime contract for readiness and capability discovery.
In native mode it reports `engine.track1_sidecar_required=false` plus a
machine-readable capability manifest for v1/v2/hybrid identity, storage safety,
jobs, migration, DHT/uTP policy, compatibility facades, metrics, and
diagnostics.

## Track 1 Sidecar

**Location:** `sidecar/`
**Binary:** `rtorrentng`

The sidecar remains a supported facade for rTorrent deployments and release
compatibility certification. It is not required by native engine mode.

### Responsibilities

- Maintain a live torrent state cache synced from rTorrent XMLRPC.
- Serve native REST and qBittorrent-compatible APIs for Track 1 users.
- Serve WebSocket events and Prometheus metrics.
- Enforce auth and script workflow policy.
- Provide migration-compatible metadata, labels, and tracker views.

### Sidecar Data Flow

```text
rTorrent ── XMLRPC poll ──► sidecar SQLite cache
                                │
                    ┌───────────┤
                    │           │
              REST clients   WebSocket
```

The sidecar is the only trusted XMLRPC client in Track 1 mode. Browser,
automation, and scripts talk to the sidecar, never directly to the SCGI socket.

## WebUI

**Location:** `webui/`
**Stack:** React 19, TypeScript strict, Vite, TanStack Query, TanStack Virtual,
TanStack Table.

The WebUI is shared by native and sidecar modes. It is built around large
libraries: virtualized rows, server-side filter/sort/page, delta events, bulk
dry-run previews, storage/tracker views, saved views, and diagnostic actions.

## Deployment

**Location:** `deploy/`

Native deployments run `rusttorrentd` with durable DB/metadata paths and storage
roots. Sidecar deployments run the Phase 1 rTorrent bundle or host rTorrent plus
`sidecar/rtorrentng`.

The release evidence is split the same way:

- `scripts/native_engine_certification_report.sh` certifies the native engine
  rewrite and can assert a live `/health` capability manifest.
- `scripts/pre_engine_certification_suite.sh` and
  `scripts/pre_engine_release_report.sh` aggregate legacy compatibility,
  integration, security, soak, and native-engine evidence.
