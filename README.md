# TorrentNG

[![Discord](https://img.shields.io/discord/5PyXBfvS6T?label=Discord&logo=discord&logoColor=white)](https://discord.gg/5PyXBfvS6T)

Support TorrentNG through [Ko-fi](https://ko-fi.com/snapetech).

TorrentNG is a BitTorrent service built around a native Rust daemon,
`torrentngd`, and a shared control plane. It provides a WebUI, native REST and
SSE APIs, compatibility adapters, migration commands, metrics, and deployment
assets. It has two runtime modes: the native TorrentNG engine and an rTorrent
sidecar for existing rTorrent deployments.

The two modes share the product-level control-plane goals, but they do not have
the same engine or state model. Choosing a mode determines where torrent work
runs, which process owns durable state, and which compatibility operations can
be engine-backed.

![TorrentNG WebUI using the Sietch Neon theme while downloading Linux ISO test data](docs/assets/torrentng-sietch-neon-linux-isos.png)

## Runtime modes

| Mode | Engine process | Authoritative state | Main surfaces | Use it for |
|---|---|---|---|---|
| Native | `torrentngd` | TorrentNG SQLite state, metainfo, and fast-resume data | WebUI, native REST/SSE, qBittorrent, Transmission, and Deluge adapters | New deployments, native peer/storage work, durable jobs, and the primary development path |
| rTorrent sidecar | `rTorrent` plus `torrentng` | rTorrent session state; sidecar cache holds projections | Sidecar WebUI/API, client adapters, and rTorrent XML-RPC/SCGI integration | Existing rTorrent sessions, staged migration, compatibility comparison, or keeping rTorrent as the engine |

In native mode, `torrentngd` performs peer, tracker, DHT, storage, job, and
session work. In sidecar mode, `torrentng` translates the control plane to
rTorrent through a trusted local SCGI/XML-RPC connection and caches the
results. The sidecar is a separate Cargo workspace and deployment path, not a
different configuration profile of the native daemon.

Read [the engine rewrite guide](docs/ENGINE_REWRITE.md) for the architectural
comparison and migration considerations.

## Native engine

`torrentngd` is the primary native daemon. It wires the engine crates, owns
startup and shutdown, exposes the native API, and reports its capability
manifest through `/health`.

The native engine currently covers:

- BitTorrent v1, v2, and hybrid metainfo parsing, identity, and metadata
  projection, including `btih` and `btmh` magnets.
- Peer-wire transfer, piece picking, HTTP and UDP trackers, DHT, webseeds, and
  policy-gated uTP transport.
- Durable SQLite session state for torrents, files, trackers, tags,
  categories, limits, events, and jobs.
- Supervised database persistence, bounded storage workers, positioned I/O,
  hashing, recheck, preallocation, move/import/delete plans, and crash
  recovery.
- Runtime tiering and memory accounting for engine, storage, peer, metadata,
  tracker, DHT, and API-snapshot allocations.
- Native Prometheus metrics and health/capability reporting.

Long-running work is represented as a durable job. The engine actor coordinates
the operation and publishes state; database and storage workers perform the
blocking persistence or filesystem work. This distinction is visible through
`/api/v1/jobs` and the storage metrics.

Pure-v2 transfer and tracker lifecycle are not implemented. Pure-v2 parsing,
identity, storage-root verification, and compatibility projections exist, but
pure-v2 metadata completion and peer transfer remain explicit unsupported
capabilities. v1 and hybrid torrents are the supported transfer paths. See
[ENGINE.md](docs/ENGINE.md) for the protocol boundary.

## Control plane and API surface

The native and sidecar deployments expose different backend implementations
behind a familiar control-plane shape. Compatibility is defined by route,
field, error, and state behavior where a workflow is implemented; it does not
make every upstream plugin or option meaningful on every backend.

| Surface | Purpose |
|---|---|
| `GET /health` | Liveness, readiness, capability, and subsystem health |
| `/api/v1` | Native REST API for torrents, session settings, storage, jobs, logs, events, and transfer state |
| `GET /api/v1/events` | Server-sent events with bounded initial data and revision-based reconnects |
| `/api/qb/v2` and `/api/v2` | qBittorrent-compatible automation and client routes |
| `/transmission/rpc` and `/api/transmission/rpc` | Transmission RPC compatibility |
| `/json` and `/deluge/json` | Deluge Web/thin-client compatibility |
| rTorrent XML-RPC library boundary | Library-level rTorrent compatibility and migration-oriented calls; this boundary is documented separately |
| `GET /metrics` | Prometheus metrics |

For native torrent lists, use the snapshot returned by
`GET /api/v1/torrents` while paging:

- `limit` defaults to 200 and is capped at 5,000.
- The response contains `snapshot`, `total`, and `torrents`.
- Send the same `snapshot` with later pages so the result set does not shift
  underneath the client.
- An expired snapshot returns `410`; start a new snapshot and reconcile.

The SSE endpoint sends bounded initial chunks and a bounded mutation journal.
Clients should retain the last known revision and resync from a fresh snapshot
when the journal no longer covers the gap. Compatibility endpoints that have
legacy full-list contracts use explicit limits: qBittorrent `sync/maindata`
rejects oversized requests rather than silently truncating, while Deluge,
Transmission, and rTorrent multicalls have documented 10,000-item safety
limits where no cursor exists.

The complete route and field matrices are in
[API.md](docs/API.md) and
[CLIENT_COMPATIBILITY_MATRICES.md](docs/CLIENT_COMPATIBILITY_MATRICES.md).

## Storage, jobs, and state

TorrentNG separates durable control-plane state from payload files.

In native mode:

- SQLite stores torrent identity, settings, lifecycle state, event history,
  and durable job state.
- Metainfo blobs and fast-resume data live under the configured session
  directory.
- Payload files live under configured storage roots.
- The supervised `DbWorker` serializes authoritative database operations while
  the storage supervisor runs bounded filesystem jobs.
- Move, import, and delete operations are planned before execution, confined
  to configured roots, checkpointed, resumable, and exposed as jobs.
- File removal with `delete_files=true` is asynchronous and returns a
  `job_id`.

This means a client should not treat a successful HTTP request as proof that a
large filesystem operation has finished. Follow the returned job through
`GET /api/v1/jobs`, and use `/api/v1/storage/plan` when a preview is needed.

The storage path includes positioned I/O, file-descriptor pooling,
preallocation, durability modes, hash-worker isolation, readahead, device
queues, and memory accounting. These are implementation controls; their
performance still depends on the filesystem, device, container limits, and
configuration. See [STORAGE_IO.md](docs/STORAGE_IO.md),
[STORAGE_NG.md](docs/STORAGE_NG.md), and
[STORAGE_MEMORY_GAP_REGISTER.md](docs/STORAGE_MEMORY_GAP_REGISTER.md).

## Quick start: native mode

The checked-in Compose file builds the native daemon and publishes its API on
host port `28082`. It expects an external Docker volume named
`certification_downloads`; create that volume or edit the Compose file for a
different payload location.

```sh
docker volume create certification_downloads
export TORRENTNG_API_TOKEN="$(openssl rand -hex 32)"
docker compose -f deploy/native/compose.yml up --build
```

Open the WebUI at `http://localhost:28082`. Check the daemon and make an
authenticated API request with:

```sh
curl -fsS http://localhost:28082/health
curl -fsS \
  -H "Authorization: Bearer ${TORRENTNG_API_TOKEN}" \
  http://localhost:28082/api/v1/torrents
```

Prometheus and Grafana can be started with the observability profile:

```sh
docker compose -f deploy/native/compose.yml --profile observability up --build
```

The native Compose config is a development and certification starting point.
For a public deployment, configure durable volumes, a real secret, a trusted
proxy or TLS, firewall rules for the peer port, and backups before importing or
moving a library. See [NATIVE_DEPLOYMENT.md](docs/NATIVE_DEPLOYMENT.md) and
[CONFIGURATION.md](docs/CONFIGURATION.md).

### Run the daemon directly

Build the native binary and point it at a configuration file:

```sh
cargo build --release -p torrentngd
TORRENTNGD_CONFIG=/etc/torrentngd/config.toml target/release/torrentngd
```

Configuration is resolved from `TORRENTNGD_CONFIG`, the user config path, or
the system config path. A non-loopback API bind requires a real API token of at
least 16 characters. The deployment examples show secret-file configuration,
storage roots, peer ports, tiering, trackers, DHT, and systemd/Kubernetes
layouts.

## Migration

`torrentngd migrate` imports existing client state into the native engine.
`torrentngd export` writes a client-specific layout from native state. Both
commands default to a read-only plan and report; `--apply` writes the result,
and the source is not modified by an import.

Start with a report:

```sh
torrentngd migrate \
  --source qbittorrent \
  --from ~/.local/share/qBittorrent/BT_backup \
  --report migration.md
```

Apply only after reviewing the report:

```sh
torrentngd migrate \
  --source qbittorrent \
  --from ~/.local/share/qBittorrent/BT_backup \
  --apply --yes
```

Supported import sources are qBittorrent, Deluge, Transmission, rTorrent,
uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati metadata, and a generic
`.torrent` directory. Path prefixes can be rewritten with repeated
`--remap OLD=NEW` options.

Export formats are `generic`, `libtorrent` (qBittorrent/Deluge),
`transmission`, `rtorrent`, `utorrent`, and `biglybt`:

```sh
torrentngd export --format libtorrent --to /tmp/qbittorrent-export
torrentngd export --format transmission --to /tmp/transmission-export --apply --yes
torrentngd export --format generic --to /tmp/generic-export --apply --yes
```

Migration fidelity is reported per torrent:

- `recheck-free` preserves enough resume state for the destination format to
  resume without a full recheck when the payload and paths match.
- `complete-only` is recheck-free for complete torrents but not for all partial
  progress.
- `metadata-only` carries torrent metadata but requires data verification.
- `torrent-only` carries `.torrent` files and a manifest; the destination must
  recheck.

The exact result depends on source state, destination format, path mapping, and
whether payload files are present. Read [MIGRATION.md](docs/MIGRATION.md)
before applying a large import or export.

## rTorrent sidecar mode

The sidecar runs the WebUI and adapter layer next to an existing rTorrent
process. It connects through SCGI/XML-RPC, translates client requests, and
keeps a cache of rTorrent state. rTorrent remains authoritative for torrent
lifecycle and payload operations in this mode.

Start the standard sidecar stack with explicit secrets:

```sh
export TNG_SECRET_KEY="$(openssl rand -hex 32)"
export TNG_API_TOKENS="$(openssl rand -hex 32)"
docker compose -f deploy/docker/compose.yml up --build
```

The nginx front door is at `http://localhost`; the sidecar API is also
published directly on `http://localhost:8080`. The default rTorrent incoming
port is `50000` TCP/UDP. The lower-level Phase 1 rTorrent/ruTorrent bundle is
available for profile testing:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

Do not share native session state and rTorrent session directories. If both
modes use the same payload files, stop one stack before starting the other and
keep their state volumes separate unless you have deliberately planned the
handoff.

## Status and evidence

TorrentNG is pre-1.0. APIs, configuration, and deployment details can change.
The repository separates implementation status from evidence that requires a
particular machine, client version, network, device, or elapsed run time.

| Area | Current interpretation |
|---|---|
| Native engine | Native unit, integration, fault, and release gates cover the implemented engine paths; target-device and long-duration behavior are separate evidence |
| WebUI | Build, lint, browser, accessibility, visual, and virtualized-table checks exist; browser coverage is not a substitute for every client workflow |
| API compatibility | Deterministic route/field/error matrices exist for native, qBittorrent, Transmission, Deluge, and rTorrent library boundaries; unsupported native operations should fail explicitly |
| Import/export | Generated corpus, apply, and round-trip tests cover the declared client formats; migration fidelity remains data- and path-dependent |
| Storage and memory | Bounded I/O, worker, durability, move/import/delete, and accounting code has local release coverage; hardware and workload results depend on the target environment |
| Interoperability and soak | Local Docker/public-client scripts and soak gates are explicit runs. A report is evidence only while its environment and timestamp remain relevant |

Use `scripts/certification_status.sh` for the current evidence roll-up. The
broader audit and burndown are in
[PROJECT_GAP_AUDIT.md](docs/PROJECT_GAP_AUDIT.md) and
[BACKEND_AUDIT_BURN_DOWN.md](docs/BACKEND_AUDIT_BURN_DOWN.md).

Known boundaries are documented rather than hidden behind successful-looking
compatibility responses:

- Pure-v2 peer transfer, tracker lifecycle, and metadata completion are
  unsupported.
- Some compatibility settings are projections for client discovery; they are
  only reported as successful when the native engine applies them.
- The rTorrent XML-RPC library entry point is a library contract with an
  explicit credential boundary, not an independently deployable HTTP server.
- Compatibility matrices describe the tested contract, not blanket parity with
  every upstream plugin, preference, or extension.

## Testing and development

Build and test the root Rust workspace:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

The rTorrent sidecar is a separate workspace:

```sh
cd sidecar
cargo test --locked
```

Build and lint the WebUI:

```sh
cd webui
npm ci
npm run build
npm run lint
```

Run the native certification and client interoperability checks from the
repository root:

```sh
scripts/native_engine_certification_report.sh
scripts/interop_matrix.sh --local
scripts/interop_matrix.sh --public
```

The local matrix uses Docker fixtures for TorrentNG, qBittorrent,
Transmission, Deluge, rTorrent, opentracker, and HTTP/webseed services. The
public matrix resolves official Debian, Ubuntu, and Fedora torrents at runtime
and downloads them by default. See [INTEROP_MATRIX.md](docs/INTEROP_MATRIX.md)
for prerequisites, coverage, and release-gate commands.

## Repository map

| Path | Purpose |
|---|---|
| `crates/` | Native engine, API, migration, metrics, and testkit crates |
| `crates/torrentngd/` | Native daemon binary ([package README](crates/torrentngd/README.md)) |
| `crates/rt-*` | Engine subsystems and native API/protocol crates |
| [`sidecar/`](sidecar/README.md) | Separate rTorrent-backed API and WebUI sidecar workspace |
| [`webui/`](webui/README.md) | React/Vite frontend shared by the runtime tracks |
| [`deploy/`](deploy/README.md) | Compose, Docker, systemd, Kubernetes, nginx, Prometheus, and Grafana assets |
| `certification/` | Checked-in certification fixtures and reports |
| [`engine-profile/`](engine-profile/README.md) | Pinned rTorrent profile and build defaults |
| `docs/` | Architecture, API, deployment, migration, security, compatibility, and roadmap documentation |
| `scripts/` | Certification, interoperability, health, release, and operations scripts |

## Documentation

Start with the [documentation index](docs/README.md). The main references are:

- [Engine rewrite guide](docs/ENGINE_REWRITE.md) — native and rTorrent mode
  architecture and when to use each
- [Native deployment](docs/NATIVE_DEPLOYMENT.md)
- [rTorrent deployment](docs/DEPLOYMENT.md)
- [Configuration](docs/CONFIGURATION.md)
- [API reference](docs/API.md)
- [Client compatibility matrices](docs/CLIENT_COMPATIBILITY_MATRICES.md)
- [Migration](docs/MIGRATION.md)
- [Interop matrix](docs/INTEROP_MATRIX.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Threat model](docs/THREAT_MODEL.md)
- [rTorrent library API boundary](docs/RTORRENT_LIBRARY_API.md)

## Support

Project support, setup help, integration discussion, and development updates
are available on [Discord](https://discord.gg/5PyXBfvS6T).

## Legal use

Users are responsible for the content they download, seed, or manage and for
complying with the laws and licenses that apply to it.

## License

TorrentNG is dual-licensed under `AGPL-3.0-or-later OR Commercial`.

Unless you have a separate signed commercial license, use is governed by the
GNU Affero General Public License v3.0 or later. See [LICENSE](LICENSE) for
details.

## Attribution

TorrentNG interoperates with rTorrent, qBittorrent-compatible clients, and
other BitTorrent ecosystem tools. Product and project names are the property
of their respective owners. This project is not affiliated with, endorsed by,
or sponsored by those projects unless explicitly stated.
