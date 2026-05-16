# rtorrentNG

[![Discord](https://img.shields.io/discord/4ub88HeHFm?label=Discord&logo=discord&logoColor=white)](https://discord.gg/4ub88HeHFm)

rtorrentNG is a torrent management stack with two runtime modes:

- A Phase 1 rTorrent sidecar for existing rTorrent deployments.
- A native Rust BitTorrent daemon (`rusttorrentd`) that owns torrent state,
  tracker announces, peer wire traffic, storage, rechecks, metrics, and native
  REST/SSE APIs.

Both modes include a React/Vite WebUI and compatibility shims for common
automation tools.

## Status

This repository is early-stage software. Interfaces, deployment details, and
runtime behavior may change while the project is being developed.

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
