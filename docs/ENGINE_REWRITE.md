# Engine Rewrite Guide

This is the practical guide to the rtorrentNG engine rewrite: what it is, how
it differs from the rTorrent-backed mode, and how to test either engine.

## Short Version

rtorrentNG has two runtime modes under one compatibility goal: make the project
usable as a migration target, an interop peer, and eventually a practical
drop-in replacement for the torrent clients and APIs operators already use.

| Mode | Process | BitTorrent engine | Best for |
|---|---|---|---|
| Native rewrite | `rusttorrentd` | rtorrentNG Rust crates | Testing and developing the new engine, native deployments, storage/recheck/job work |
| rTorrent core | `rTorrent` + `rtorrentng` sidecar | Upstream rTorrent/libtorrent | Compatibility comparison, migration bridge, existing rTorrent users |

The WebUI and compatibility APIs are shared goals, but the source of truth is
different. Native mode owns the torrent lifecycle itself. rTorrent mode reflects
and controls rTorrent through a trusted local SCGI/XMLRPC socket. The
compatibility matrix is the contract for closing the remaining gap between
"works with common flows" and "universal in/out compatibility."

## Why the Rewrite Exists

The Track 1 sidecar fixed the control plane around rTorrent: auth, WebUI,
qBittorrent-compatible endpoints, safer local RPC access, integration tests,
and operational profiles. It could not fix the engine-level constraints:

- rTorrent owns storage behavior and has no rtorrentNG userspace disk scheduler.
- Rechecks are not durable rtorrentNG jobs with pause/resume/cancel semantics.
- Torrent lifecycle history is limited compared with native structured events.
- The sidecar must poll and translate XMLRPC state.
- Engine behavior depends on rTorrent/libtorrent build details.
- BEP 52/v2, compatibility facades, migration, and metrics are easier to make
  consistent when projected from one native model.

The rewrite moves those concerns into Rust crates with durable SQLite state,
explicit storage scheduling, observable jobs, and API projections over one
engine model. That single model is what makes the larger compatibility target
credible: import from many clients, project many APIs, and certify behavior from
one source of truth instead of binding rtorrentNG to one legacy engine.

## Native Engine Shape

`rusttorrentd` wires the native crates into one daemon:

```text
WebUI / automation clients
          |
          v
native REST, SSE, qBit, Transmission, Deluge facades
          |
          v
rt-engine registry and torrent supervisors
          |
          +-- rt-db SQLite state, events, settings, jobs
          +-- rt-storage roots, mount awareness, safe move/import, rechecks
          +-- rt-tracker HTTP/UDP announces, tiers, backoff, scrape state
          +-- rt-peer-wire / rt-peer-manager / rt-piece-picker
          +-- rt-dht / rt-utp policy and protocol crates
          +-- rt-migrate importers for existing client state
```

The native engine treats the database and metadata store as authoritative.
Startup restores torrents from that durable state and projects the same data to
native REST, qBittorrent, Transmission, Deluge, metrics, and the WebUI.

For deeper design details, see [ENGINE.md](ENGINE.md). For crate-level status,
see [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md).

## rTorrent Core Shape

The rTorrent-backed mode keeps upstream rTorrent as the engine:

```text
WebUI / automation clients
          |
          v
sidecar/rtorrentng
          |
          v
trusted local SCGI/XMLRPC socket
          |
          v
rTorrent/libtorrent session and storage
```

The sidecar is a facade. It caches state, serves the WebUI, exposes native and
qBittorrent-compatible endpoints, enforces auth, and reports metrics, but it
does not own peer wire traffic, storage scheduling, or rTorrent session state.

## Swapping Engines for Local Testing

Use separate state volumes when switching modes. The two engines can point at
the same payload directory for careful migration or comparison, but they should
not share a session database/session directory.

The native and sidecar Compose examples both bind host port `8080`, so stop one
stack before starting the other unless you intentionally override ports or use
separate Compose projects.

### Test the Native Rewrite

Start native mode:

```sh
docker compose -f deploy/native/compose.yml up --build
```

Default endpoints:

| Endpoint | Purpose |
|---|---|
| `http://localhost:8080/health` | Native readiness and capability manifest |
| `http://localhost:8080/api/v1/torrents` | Native torrent list |
| `http://localhost:8080/api/qb/v2/torrents/info` | qBittorrent-compatible list |
| `http://localhost:8080/metrics` | Prometheus metrics |

Local binary flow:

```sh
cargo build --bin rusttorrentd
cp deploy/native/config.toml /tmp/rusttorrentd.config.toml
$EDITOR /tmp/rusttorrentd.config.toml
RUSTTORRENTD_CONFIG=/tmp/rusttorrentd.config.toml target/debug/rusttorrentd
```

For local binary runs, change `session_dir`, `[db].path`, and
`[storage].download_dir` to paths your user can write. The checked-in native
config is shaped for the container.

Run the native certification gate:

```sh
scripts/native_engine_certification_report.sh
NATIVE_ENGINE_URL=http://127.0.0.1:8080 scripts/native_engine_certification_report.sh
```

### Test the rTorrent Core Through the Sidecar

Start the rTorrent plus sidecar stack:

```sh
docker compose -f deploy/docker/compose.yml up --build
```

Default endpoints:

| Endpoint | Purpose |
|---|---|
| `http://localhost:8080/health` | Sidecar and rTorrent reachability |
| `http://localhost:8080/api/v1/engine` | rTorrent provenance, XMLRPC capabilities, profile drift |
| `http://localhost:8080/api/qb/v2/torrents/info` | qBittorrent-compatible list |
| `http://localhost:80/` | nginx front door for the sidecar/WebUI stack |

The sidecar talks to rTorrent through `/run/rtorrent/rpc.sock` inside the
container. Do not expose the SCGI socket to untrusted networks.

### Test Only the Phase 1 rTorrent Profile

Use this when you need the low-level rTorrent/ruTorrent bundle without the
newer sidecar-first stack:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

This exercises the pinned rTorrent/libtorrent profile and ruTorrent packaging.
It is useful for checking `engine-profile/rtorrent.rc`, incoming ports, DHT
settings, and SCGI socket behavior.

## What Changes Between Engines

| Area | Native rewrite | rTorrent core |
|---|---|---|
| Source of truth | `rusttorrentd` SQLite state and metadata store | rTorrent session directory and libtorrent runtime |
| Control plane | Native REST/SSE plus compat APIs over engine state | Sidecar REST/WebUI/compat APIs over XMLRPC cache |
| Peer traffic | Native Rust peer wire tasks | rTorrent/libtorrent |
| Tracker state | Native tracker manager with persisted announce state | rTorrent tracker stack reflected through XMLRPC |
| Storage | `rt-storage` roots, mount awareness, scheduler, safe plans | rTorrent/libtorrent storage behavior |
| Rechecks | Durable jobs with progress and recovery | rTorrent commands surfaced through sidecar |
| Events | Structured native engine events | Sidecar events derived from polling/cache changes |
| Metrics | Native engine, storage, jobs, API metrics | Sidecar sync/API metrics plus rTorrent-derived state |
| Migration | Native importers in `rt-migrate` | Existing rTorrent session remains in place |
| Best risk profile | New engine behavior under active certification | Mature rTorrent behavior with wrapper limitations |

## Testing Both Engines Against the Same Scenario

For a controlled comparison:

1. Use separate Compose projects or clear volumes between runs.
2. Keep the same payload fixture, torrent file, incoming port assumptions, and
   API client action.
3. Run native mode and capture `/health`, `/metrics`, native list, qBit list,
   and logs.
4. Run rTorrent sidecar mode and capture `/health`, `/api/v1/engine`, qBit
   list, and logs.
5. Compare completion state, tracker messages, peer counts, file verification,
   API response shape, and restart recovery.

The interop matrix automates much of this across `rusttorrentd`, qBittorrent,
Transmission, Deluge, and rTorrent:

```sh
scripts/interop_matrix.sh
```

It uses [deploy/interop/compose.yml](../deploy/interop/compose.yml) and writes
reports and logs under the configured certification work directory.

## Migration Notes

Do not treat engine swapping as an in-place state file conversion. Use import
and verification:

- Back up the rTorrent session directory and native `rusttorrentd` session DB.
- Import existing rTorrent state through the migration flow in
  [MIGRATION.md](MIGRATION.md).
- Point native mode at existing payload paths only when you intend to verify and
  seed those files.
- Keep the old rTorrent session until native restart recovery, recheck, tracker
  announces, and automation client flows have passed.

## Documentation Pointers

- [NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md) covers production native mode.
- [DEPLOYMENT.md](DEPLOYMENT.md) covers rTorrent plus sidecar mode.
- [CONFIGURATION.md](CONFIGURATION.md) lists both config surfaces.
- [ARCHITECTURE.md](ARCHITECTURE.md) shows how runtime components fit together.
- [BACKUP_RESTORE.md](BACKUP_RESTORE.md) covers native state backup.
- [API.md](API.md) documents native and compatibility endpoints.
