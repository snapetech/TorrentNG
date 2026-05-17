# rtorrentNG

[![Discord](https://img.shields.io/discord/4ub88HeHFm?label=Discord&logo=discord&logoColor=white)](https://discord.gg/4ub88HeHFm)

rtorrentNG is a torrent management stack with two runtime modes. The native
engine rewrite is the primary path; the rTorrent-backed sidecar remains for
comparison, migration, and users who still want the upstream rTorrent core.

- A native Rust BitTorrent daemon (`rusttorrentd`) that owns torrent state,
  tracker announces, peer wire traffic, storage, rechecks, metrics, and native
  REST/SSE APIs.
- A Track 1 rTorrent sidecar for existing rTorrent deployments and migration
  compatibility.

Both modes include a React/Vite WebUI and compatibility shims for common
automation tools. The API shape is intentionally similar, but the engine below
it is different.

## Status

The native engine rewrite is the primary runtime path. The rewrite surface is
tracked in [docs/ENGINE_REWRITE_BURNDOWN.md](docs/ENGINE_REWRITE_BURNDOWN.md)
and certified by `scripts/native_engine_certification_report.sh`. Interfaces,
deployment details, and runtime behavior may still change before a 1.0 release.

Read [docs/ENGINE_REWRITE.md](docs/ENGINE_REWRITE.md) for the practical guide
to the rewrite: what changed, how native mode differs from the rTorrent core,
and how to swap between them for testing.

## Choosing an Engine

Use native mode when you want to test the rewrite:

```sh
docker compose -f deploy/native/compose.yml up --build
```

Native mode runs `rusttorrentd`. It owns session state, tracker state, peer
connections, storage scheduling, rechecks, metrics, and the native and
compatibility APIs.

Use the rTorrent core when you want to compare behavior or keep the historical
runtime:

```sh
docker compose -f deploy/docker/compose.yml up --build
```

That mode runs rTorrent plus the `rtorrentng` sidecar. rTorrent remains the
BitTorrent engine; the sidecar bridges local SCGI/XMLRPC into the WebUI,
native REST facade, qBittorrent-compatible endpoints, auth, cache, and metrics.

The pure Phase 1 ruTorrent/rTorrent bundle is still available for low-level
rTorrent profile testing:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

Keep separate volumes or session directories when switching modes against the
same payload data. Native mode stores durable state in `rusttorrentd` SQLite
state; rTorrent mode stores state in the rTorrent session directory.

## Support

Project support, setup help, integration discussion, and development updates
are available on Discord: [https://discord.gg/4ub88HeHFm](https://discord.gg/4ub88HeHFm).

## Components

- `sidecar/` - Rust service for rTorrent deployments. It talks to rTorrent over
  a trusted local SCGI socket, maintains a SQLite cache, serves APIs, and
  exposes qBittorrent-compatible endpoints.
- `crates/` - Native Rust engine crates plus `rusttorrentd`, the standalone
  native daemon.
- `webui/` - React, TypeScript, and Vite frontend for torrent management.
- `engine-profile/` - rTorrent configuration profile and operational defaults.
- `deploy/` - Docker, Compose, systemd, and nginx deployment examples.
- `docs/` - API, architecture, configuration, engine, migration, and roadmap
  notes.

## Documentation Map

- [Engine rewrite guide](docs/ENGINE_REWRITE.md) - practical overview, swap
  workflows, native-vs-rTorrent differences, and testing checklist.
- [Native engine design](docs/ENGINE.md) - deeper architecture and crate-level
  design for the rewrite.
- [Architecture](docs/ARCHITECTURE.md) - how native mode, sidecar mode, WebUI,
  APIs, and certification fit together.
- [Native deployment](docs/NATIVE_DEPLOYMENT.md) - production `rusttorrentd`
  setup.
- [Track 1 deployment](docs/DEPLOYMENT.md) - rTorrent plus sidecar setup.
- [Migration](docs/MIGRATION.md) - importing existing rTorrent and other client
  state.

## Legal Use

rtorrentNG is a torrent management interface for lawful content distribution,
personal data management, and legitimate automation workflows. The project does
not condone piracy, copyright infringement, or using BitTorrent to access or
distribute content without the right to do so. Users are responsible for
understanding and following the laws and licenses that apply to the content
they download, seed, or manage.

## License

rtorrentNG is dual-licensed under `AGPL-3.0-or-later OR Commercial`.

Unless you have a separate signed commercial license, your use of this software
is governed by the GNU Affero General Public License v3.0 or later. Commercial
licensing is available for users who need terms outside the AGPL.

See [LICENSE](LICENSE) for details.

## Attribution

rtorrentNG is built around interoperability with rTorrent and common automation
tools in the BitTorrent ecosystem. Product and project names mentioned in this
repository are trademarks or property of their respective owners. This project
is not affiliated with, endorsed by, or sponsored by rTorrent, qBittorrent, or
the maintainers of third-party automation tools unless explicitly stated.

## Development

Build the web UI:

```sh
cd webui
npm install
npm run build
```

Build the native daemon and sidecar crates:

```sh
cargo build
```

Run Rust tests:

```sh
cargo test
```

Run native engine certification:

```sh
scripts/native_engine_certification_report.sh
```

Run the cross-client interop matrix when comparing the rewrite against rTorrent
and other clients:

```sh
scripts/interop_matrix.sh
```
