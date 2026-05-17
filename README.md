# rtorrentNG

[![Discord](https://img.shields.io/discord/4ub88HeHFm?label=Discord&logo=discord&logoColor=white)](https://discord.gg/4ub88HeHFm)

rtorrentNG is a modern torrent management stack for headless servers. It
provides a native Rust BitTorrent daemon, a React WebUI, automation-friendly
APIs, and a compatibility bridge for existing rTorrent deployments.

The project currently supports two engine modes:

| Mode | Process | Source of truth | Use when |
|---|---|---|---|
| Native engine | `rusttorrentd` | rtorrentNG Rust engine and SQLite state | You want the primary rewrite path, native storage/recheck/jobs, and one model for WebUI plus APIs |
| rTorrent sidecar | `rTorrent` + `rtorrentng` | rTorrent session state | You need an rTorrent migration bridge, compatibility comparison, or an rTorrent-backed deployment |

Both modes aim to expose the same user-facing WebUI and compatibility surfaces,
including qBittorrent-style endpoints used by common automation tools.

![rtorrentNG WebUI using the Sietch Neon theme while downloading Linux ISO test data](docs/assets/rtorrentng-sietch-neon-linux-isos.png)

Screenshot uses mocked Linux ISO torrent data, not a live user session.

## Status

The native Rust engine is the primary development and deployment path. The
rTorrent-backed sidecar remains supported for migration, comparison, and users
who still want the upstream rTorrent core.

rtorrentNG is pre-1.0 software. APIs, configuration, and deployment details can
still change. Track current native-engine work in
[docs/ENGINE_REWRITE_BURNDOWN.md](docs/ENGINE_REWRITE_BURNDOWN.md), and use
[docs/ENGINE_REWRITE.md](docs/ENGINE_REWRITE.md) for the practical native vs.
rTorrent guide.

## Quick Start

Start the native engine stack:

```sh
docker compose -f deploy/native/compose.yml up --build
```

Open the WebUI at:

```text
http://localhost:8080
```

Useful native endpoints:

```text
http://localhost:8080/health
http://localhost:8080/api/v1/torrents
http://localhost:8080/api/qb/v2/torrents/info
http://localhost:8080/metrics
```

Add observability with Prometheus and Grafana:

```sh
docker compose -f deploy/native/compose.yml --profile observability up --build
```

## rTorrent Mode

Start the rTorrent-backed stack when you need the historical engine path:

```sh
docker compose -f deploy/docker/compose.yml up --build
```

This runs rTorrent plus the `rtorrentng` sidecar. The sidecar talks to rTorrent
over a trusted local SCGI/XMLRPC socket, maintains a cache, serves the WebUI,
and exposes native and qBittorrent-compatible APIs.

The lower-level Phase 1 rTorrent/ruTorrent bundle is still available for
rTorrent profile testing:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

Do not share native session state and rTorrent session directories. If you test
both modes against the same payload data, keep separate state volumes and stop
one stack before starting the other unless you intentionally change ports.

## What Is Included

- Native Rust engine crates for bencode, metainfo, hashing, storage, trackers,
  DHT, uTP, peer wire, piece picking, session state, jobs, migration, metrics,
  and API projections.
- `rusttorrentd`, the native daemon that owns torrent state and serves APIs.
- `sidecar/rtorrentng`, the rTorrent facade for existing deployments.
- React, TypeScript, and Vite WebUI in `webui/`.
- Docker Compose, Dockerfile, systemd, Kubernetes, nginx, Prometheus, and
  Grafana examples under `deploy/`.
- Certification, interoperability, security review, and soak scripts under
  `scripts/`.

## Repository Map

| Path | Purpose |
|---|---|
| `crates/` | Native engine, API, migration, metrics, and testkit crates |
| `crates/rusttorrentd/` | Native daemon binary |
| `sidecar/` | rTorrent-backed API/WebUI sidecar |
| `webui/` | React/Vite frontend |
| `deploy/native/` | Native engine Compose, Docker, systemd, Kubernetes, metrics assets |
| `deploy/docker/` | rTorrent sidecar and Phase 1 rTorrent/ruTorrent deployment assets |
| `deploy/certification/` | Integration certification stack |
| `engine-profile/` | rTorrent profile and operational defaults |
| `docs/` | Architecture, API, deployment, migration, security, and roadmap docs |
| `scripts/` | Certification, interop, health, release, and operations scripts |

## Development

Build the native Rust workspace:

```sh
cargo build
```

Run Rust tests:

```sh
cargo test
```

Build the rTorrent sidecar:

```sh
cd sidecar
cargo build
```

Build the WebUI:

```sh
cd webui
npm install
npm run build
```

Run WebUI linting:

```sh
cd webui
npm run lint
```

Run native certification:

```sh
scripts/native_engine_certification_report.sh
```

Run the cross-client interoperability matrix:

```sh
scripts/interop_matrix.sh --local
scripts/interop_matrix.sh --public
```

The matrix runs `rusttorrentd` beside qBittorrent, Transmission, Deluge,
rTorrent, opentracker, and a fixture HTTP server. Local mode verifies
deterministic client-to-client transfers, webseeds, explicit private peers,
restart recovery, churn, protocol rows for UDP trackers and qBit mutation
compatibility, experimental magnet coverage, and API facade health. Public mode
resolves official Debian, Ubuntu, and Fedora torrents at runtime and fully
downloads them by default. See [docs/INTEROP_MATRIX.md](docs/INTEROP_MATRIX.md)
for the full coverage table and release-gate commands.

## Documentation

Start with the docs index:

- [Docs index](docs/README.md)
- [Engine rewrite guide](docs/ENGINE_REWRITE.md)
- [Native deployment](docs/NATIVE_DEPLOYMENT.md)
- [Track 1 rTorrent deployment](docs/DEPLOYMENT.md)
- [Configuration](docs/CONFIGURATION.md)
- [API reference](docs/API.md)
- [Interop matrix](docs/INTEROP_MATRIX.md)
- [Migration](docs/MIGRATION.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Threat model](docs/THREAT_MODEL.md)

## Support

Project support, setup help, integration discussion, and development updates are
available on Discord:

```text
https://discord.gg/4ub88HeHFm
```

## Legal Use

rtorrentNG is intended for lawful content distribution, personal data
management, and legitimate automation workflows. The project does not condone
copyright infringement or unauthorized access to content. Users are responsible
for understanding and following the laws and licenses that apply to the content
they download, seed, or manage.

## License

rtorrentNG is dual-licensed under `AGPL-3.0-or-later OR Commercial`.

Unless you have a separate signed commercial license, your use of this software
is governed by the GNU Affero General Public License v3.0 or later. Commercial
licensing is available for users who need terms outside the AGPL.

See [LICENSE](LICENSE) for details.

## Attribution

rtorrentNG interoperates with rTorrent, qBittorrent-compatible clients, and
common automation tools in the BitTorrent ecosystem. Product and project names
mentioned in this repository are trademarks or property of their respective
owners. This project is not affiliated with, endorsed by, or sponsored by
rTorrent, qBittorrent, or third-party automation projects unless explicitly
stated.
