# Certification Stack

This directory contains the local live-certification harness for Track 1
rTorrent-sidecar integrations. Native-engine certification lives beside it in
`scripts/native_engine_certification_report.sh`, and cross-client native
interop is covered by `scripts/interop_matrix.sh`.

The Track 1 stack starts rtorrentNG alongside Sonarr, Radarr, Prowlarr, autobrr,
and cross-seed. The runner verifies that the rTorrent-backed sidecar
qBittorrent-compatible API is reachable from the same Docker network and writes
a markdown report under `certification/reports/`.

## Run

```sh
cp deploy/certification/.env.example deploy/certification/.env
docker compose --env-file deploy/certification/.env -f deploy/certification/compose.yml up -d --build
./scripts/live_certification.sh
```

Set `CERT_START_STACK=1` to have the runner start the stack before probing it:

```sh
CERT_START_STACK=1 ./scripts/live_certification.sh
```

Configure the live Sonarr, Radarr, Prowlarr, and autobrr containers to use rtorrentNG as a qBittorrent-compatible download client:

```sh
./scripts/configure_certification_clients.sh
```

Run a real transfer fixture through a disposable local tracker and stock Transmission seeder:

```sh
./scripts/live_transfer_certification.sh
```

The transfer runner creates a small local torrent in the Docker downloads volume, starts `opentracker` and `transmission-cli` sidecars on the certification network, adds the torrent through rtorrentNG's qBittorrent-compatible API, and waits for completion. It also adds a public Debian netinst torrent in stopped mode as an external torrent-file smoke test. Set `PUBLIC_TRANSFER=1` to let the public Linux torrent download.

Run a transfer churn soak when you need repeated add/download/delete pressure
instead of only synthetic cached rows:

```sh
TRANSFER_CHURN_CYCLES=25 ./scripts/transfer_churn_soak.sh
```

The churn runner creates a fresh legal fixture torrent per cycle, seeds it from
a stock Transmission sidecar, adds it through rtorrentNG, waits for completion,
deletes the torrent and files, and samples RSS after each cycle. Set
`TRANSFER_CHURN_PUBLIC_CYCLES=1` or higher to also cycle a public Debian
netinst torrent from `PUBLIC_TORRENT_URL`.

Run the larger release-grab gate in a separate normal-sync compose project:

```sh
./scripts/release_grab_certification.sh
```

That runner uses non-conflicting host ports, configures the app clients, then proves Prowlarr, Sonarr, and Radarr can search local Torznab fixtures, submit release grabs through their own APIs, and complete transfers through rtorrentNG. It tears the temporary stack down by default; set `CERT_GRAB_KEEP_STACK=1` to keep it for debugging.

Run the Docker client interop matrix against rusttorrentd, qBittorrent,
Transmission, Deluge, rTorrent, and opentracker:

```sh
./scripts/interop_matrix.sh --local
./scripts/interop_matrix.sh --public
./scripts/interop_matrix.sh --all
```

The local mode creates deterministic fixture torrents and verifies file hashes
across client-to-client transfers. The public mode resolves Debian, Ubuntu, and
Fedora torrents from official project indexes at runtime and fully downloads
them by default. Set `INTEROP_INCLUDE_LIBREOFFICE=1` to include the optional
LibreOffice entry when its official torrent is available. The full matrix,
environment reference, report format, and release-gate expectations are
documented in [docs/INTEROP_MATRIX.md](../../docs/INTEROP_MATRIX.md).

Run native-engine certification directly:

```sh
./scripts/native_engine_certification_report.sh
NATIVE_ENGINE_URL=http://127.0.0.1:8080 ./scripts/native_engine_certification_report.sh
```

Refresh the short automated gate set and write a consolidated release report:

```sh
./scripts/pre_engine_certification_suite.sh
./scripts/pre_engine_release_report.sh
```

Certify the Phase 1 ruTorrent bundle when its container is running:

```sh
./scripts/phase1_certification.sh
```

Inspect and finalize the long soak:

```sh
./scripts/soak_status.sh
./scripts/post_soak_release_gate.sh
```

`post_soak_release_gate.sh` is intended for after the 24-hour soak has completed. It finalizes the soak with `RESTORE_NORMAL=1`, reruns the short certification suite, and refreshes the consolidated release report.

The readiness runner verifies container/API readiness plus the Track 1
sidecar/qBit compatibility surface. The client-configuration script completes
the first-run qBittorrent client setup through each app's API and records
whether each app's own connection test accepts rtorrentNG.

## Host ports

| Host | Container | Service |
|---|---:|---|
| `18080` | `8080` | rtorrentNG sidecar/WebUI |
| `18989` | `8989` | Sonarr |
| `17878` | `7878` | Radarr |
| `19696` | `9696` | Prowlarr |
| `17474` | `7474` | autobrr |
| `12468` | `2468` | cross-seed |
