# TorrentNG Client and Compatible-Client Integration Guide

This is the practical guide to the two ways to run TorrentNG: with an existing
compatible torrent client, or with TorrentNG's own next-generation client. It
explains the ownership boundary, why the first-party client exists, and how to
test or migrate between arrangements.

## Short Version

TorrentNG has one WebUI/API product with two backend arrangements. The goal is
to let operators keep the clients and automation tools they already use, while
providing a first-party client that can take over when TorrentNG should own the
transfer stack.

| Arrangement | TorrentNG service | Transfer owner | Best for |
|---|---|---|---|
| Compatible-client integration | `torrentng` (implementation in `sidecar/`) | An existing rTorrent, qBittorrent, Transmission, Deluge, or separate `torrentngd` client | Keep an existing library and client, add TorrentNG's WebUI/API, or migrate gradually |
| TorrentNG client | `torrentngd` | TorrentNG's first-party Rust client | New deployments and a next-generation client with owned storage, persistence, jobs, rechecks, and protocol control |

The WebUI and compatibility APIs are shared. In a compatible-client
integration, the selected client owns torrent lifecycle, peer traffic, payload
storage, and session state; `torrentng` translates and caches that state. In
the TorrentNG client arrangement, `torrentngd` owns those responsibilities and
serves the same WebUI/API directly.

Compatibility is capability-aware. The matrices define the supported route,
field, error, and state behavior for each client; they do not claim that every
upstream plugin or option is meaningful on every backend.

## Why the TorrentNG Client Exists

The compatible-client integration fixes the control plane around an existing
client: auth, WebUI, qBittorrent-compatible endpoints, safer RPC access,
integration tests, and operational profiles. It does not replace the selected
client's transfer engine or remove its constraints:

- rTorrent owns storage behavior and has no TorrentNG userspace disk scheduler.
- Rechecks are not durable TorrentNG jobs with pause/resume/cancel semantics.
- Torrent lifecycle history is limited compared with TorrentNG's structured
  events.
- The `torrentng` integration service must poll and translate XMLRPC state.
- Engine behavior depends on rTorrent/libtorrent build details.
- BEP 52/v2, compatibility facades, migration, and metrics are easier to make
  consistent when projected from one first-party model.

The TorrentNG client moves those concerns into Rust crates with durable SQLite
state, explicit storage scheduling, observable jobs, and API projections over
one client model. That is better for deployments that need TorrentNG to own
storage and persistence, recover predictably after crashes, resume or cancel
long-running work, and apply first-party protocol controls consistently. It also
makes the broader compatibility target easier to extend: import from many
clients, project many APIs, and certify behavior from one source of truth
without binding the first-party path to a legacy client.

## TorrentNG Client Shape

`torrentngd` wires the TorrentNG transfer crates into one client daemon:

```text
WebUI / automation clients
          |
          v
TorrentNG WebUI/API surface
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

The TorrentNG client treats the database and metadata store as authoritative.
Startup restores torrents from that durable state and projects the same data to
TorrentNG REST, qBittorrent, Transmission, Deluge, metrics, and the WebUI.

For deeper design details, see [ENGINE.md](ENGINE.md). For crate-level status,
see [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md).

## Compatible-Client Integration Shape

The compatible-client arrangement keeps the selected external client as the
transfer owner. For rTorrent, the flow is:

```text
WebUI / automation clients
          |
          v
torrentng compatible-client service (`sidecar/`)
          |
          v
trusted local SCGI/XMLRPC socket
          |
          v
rTorrent/libtorrent session and storage
```

The service is a WebUI/API and compatibility facade. It caches state, exposes
TorrentNG and qBittorrent-compatible endpoints, enforces auth, and reports
metrics, but it does not own peer-wire traffic, storage scheduling, or the
selected client's session state. The same pattern applies to qBittorrent,
Transmission, and Deluge adapters.

## Comparing Arrangements for Local Testing

Use separate state volumes when switching arrangements. The two clients can
point at the same payload directory for careful migration or comparison, but
they should not share a session database/session directory.

The TorrentNG-client and compatible-client Compose examples both bind host port
`8080`, so stop one stack before starting the other unless you intentionally
override ports or use separate Compose projects.

### Test the TorrentNG Client

Start the TorrentNG client:

```sh
docker compose -f deploy/native/compose.yml up --build
```

Default endpoints:

| Endpoint | Purpose |
|---|---|
| `http://localhost:8080/health` | TorrentNG-client readiness and capability manifest |
| `http://localhost:8080/api/v1/torrents` | TorrentNG-client torrent list |
| `http://localhost:8080/api/qb/v2/torrents/info` | qBittorrent-compatible list |
| `http://localhost:8080/metrics` | Prometheus metrics |

Local binary flow:

```sh
cargo build --bin torrentngd
cp deploy/native/config.toml /tmp/torrentngd.config.toml
$EDITOR /tmp/torrentngd.config.toml
TORRENTNGD_CONFIG=/tmp/torrentngd.config.toml target/debug/torrentngd
```

For local binary runs, change `session_dir`, `[db].path`, and
`[storage].download_dir` to paths your user can write. The checked-in
TorrentNG-client config is shaped for the container.

Run the TorrentNG-client certification gate:

```sh
scripts/native_engine_certification_report.sh
NATIVE_ENGINE_URL=http://127.0.0.1:8080 scripts/native_engine_certification_report.sh
```

### Test a Compatible rTorrent Client Through the Integration Service

Start the rTorrent plus compatible-client stack:

```sh
docker compose -f deploy/docker/compose.yml up --build
```

Default endpoints:

| Endpoint | Purpose |
|---|---|
| `http://localhost:8080/health` | Integration service and rTorrent reachability |
| `http://localhost:8080/api/v1/engine` | rTorrent provenance, XMLRPC capabilities, profile drift |
| `http://localhost:8080/api/qb/v2/torrents/info` | qBittorrent-compatible list |
| `http://localhost:80/` | nginx front door for the compatible-client/WebUI stack |

The integration service talks to rTorrent through
`/run/rtorrent/rpc.sock` inside the container. Do not expose the SCGI socket to
untrusted networks.

### Test Only the Phase 1 rTorrent Profile

Use this when you need the low-level rTorrent/ruTorrent bundle without the
newer compatible-client stack:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

This exercises the pinned rTorrent/libtorrent profile and ruTorrent packaging.
It is useful for checking `engine-profile/rtorrent.rc`, incoming ports, DHT
settings, and SCGI socket behavior.

## What Changes Between Arrangements

| Area | TorrentNG client | Compatible rTorrent client |
|---|---|---|
| Source of truth | `torrentngd` SQLite state and metadata store | rTorrent session directory and libtorrent runtime |
| Control plane | Shared WebUI/API served directly by `torrentngd` | `torrentng` WebUI/API service over an XMLRPC cache |
| Peer traffic | TorrentNG Rust peer-wire tasks | rTorrent/libtorrent |
| Tracker state | TorrentNG tracker manager with persisted announce state | rTorrent tracker stack reflected through XMLRPC |
| Storage | `rt-storage` roots, mount awareness, scheduler, safe plans | rTorrent/libtorrent storage behavior |
| Rechecks | Durable jobs with progress and recovery | rTorrent commands surfaced through the integration service |
| Events | Structured TorrentNG client events | Integration-service events derived from polling/cache changes |
| Metrics | TorrentNG engine, storage, jobs, API metrics | Integration sync/API metrics plus rTorrent-derived state |
| Migration | TorrentNG-client importers in `rt-migrate` | Existing rTorrent session remains in place |
| Best fit | TorrentNG owns storage, persistence, and protocol behavior | Keep mature rTorrent behavior and an existing library |

## Testing Both Arrangements Against the Same Scenario

For a controlled comparison:

1. Use separate Compose projects or clear volumes between runs.
2. Keep the same payload fixture, torrent file, incoming port assumptions, and
   API client action.
3. Run the TorrentNG client and capture `/health`, `/metrics`, the TorrentNG list,
   the qBit list, and logs.
4. Run the compatible rTorrent integration and capture `/health`,
   `/api/v1/engine`, the qBit list, and logs.
5. Compare completion state, tracker messages, peer counts, file verification,
   API response shape, and restart recovery.

The interop matrix automates much of this across `torrentngd`, qBittorrent,
Transmission, Deluge, and rTorrent:

```sh
scripts/interop_matrix.sh
```

It uses [deploy/interop/compose.yml](../deploy/interop/compose.yml) and writes
reports and logs under the configured certification work directory.

## Migration Notes

Do not treat switching clients as an in-place state-file conversion. Use import
and verification:

- Back up the rTorrent session directory and TorrentNG client session DB.
- Import existing rTorrent state through the migration flow in
  [MIGRATION.md](MIGRATION.md).
- Point `torrentngd` at existing payload paths only when you intend to verify
  and seed those files.
- Keep the old rTorrent session until TorrentNG-client restart recovery,
  recheck, tracker announces, and automation client flows have passed.

## Documentation Pointers

- [TorrentNG client deployment guide](NATIVE_DEPLOYMENT.md) covers production
  TorrentNG client deployments.
- [DEPLOYMENT.md](DEPLOYMENT.md) covers compatible-client deployments.
- [CONFIGURATION.md](CONFIGURATION.md) lists both configuration surfaces.
- [ARCHITECTURE.md](ARCHITECTURE.md) shows how the shared WebUI/API and backend
  arrangements fit together.
- [BACKUP_RESTORE.md](BACKUP_RESTORE.md) covers TorrentNG-client state backup.
- [API.md](API.md) documents the shared TorrentNG REST and compatibility endpoints.
