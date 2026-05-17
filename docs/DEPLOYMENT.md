# Deployment

This document covers the Track 1 rTorrent and sidecar deployment path. For the
native `rusttorrentd` engine, use [NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md).
For the engine rewrite overview and swap/testing workflow, use
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

## Phase 1 bundle

The Phase 1 bundle packages rTorrent 0.16.11, libtorrent 0.16.11, ruTorrent 5.3.1, nginx, PHP-FPM, and the TorrentNG engine profile.

Build and start:

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

Default ports:

| Host port | Container | Purpose |
|---|---|---|
| `8080` | `80/tcp` | ruTorrent |
| `50000` | `50000/tcp` | BitTorrent incoming TCP |
| `50000` | `50000/udp` | BitTorrent incoming UDP |

Volumes:

| Volume | Container path | Purpose |
|---|---|---|
| `downloads` | `/data` | Download data |
| `session` | `/session` | rTorrent session state |
| `./config` | `/config` | Optional `rtorrent.rc` overlay |

Put site-specific rTorrent overrides in:

```text
deploy/docker/config/rtorrent.rc
```

The container imports `/etc/rtorrent/rtorrent.rc`, which imports `engine-profile/rtorrent.rc`, then imports `/etc/rtorrent/user.rc` when an overlay exists.

## Diagnostics

From the host:

```sh
./scripts/healthcheck.sh /run/rtorrent/rpc.sock http://localhost:8080 http://localhost:8080/rutorrent/
```

From inside the Phase 1 container:

```sh
/scripts/healthcheck.sh /run/rtorrent/rpc.sock http://localhost:8080 http://localhost/rutorrent/
```

## Sidecar config

`deploy/docker/sidecar.config.toml` is a container-oriented sidecar config that points at the Phase 1 socket path and `/data` storage root.

For a host install, copy the same shape to:

```text
~/.config/torrentng/config.toml
```

## Sidecar container

The main Dockerfile builds the Rust sidecar and React WebUI, starts rTorrent, and serves the WebUI from the sidecar process.

```sh
docker compose -f deploy/docker/compose.yml up --build
```

The entrypoint creates `/config/config.toml` from `deploy/docker/sidecar.config.toml` when no config file exists. Set `TNG_SECRET_KEY` and `TNG_API_TOKENS` in the compose environment for production auth; `TNG_API_TOKENS` is a comma-separated list for automation clients. Override `TNG_STATIC_DIR` only if you mount WebUI assets somewhere other than `/usr/share/torrentng/webui`.

### Home live-main updater

For a home test instance that should follow GitHub `main`, run the updater from
the host instead of trying to mutate the running container. The Docker image must
still be rebuilt because it contains the compiled Rust sidecar, built WebUI
assets, and packaged rTorrent/libtorrent binaries.

Install the user timer:

```sh
mkdir -p ~/.config/systemd/user
cp deploy/systemd/torrentng-live-main-update.service ~/.config/systemd/user/
cp deploy/systemd/torrentng-live-main-update.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now torrentng-live-main-update.timer
```

The sample unit assumes this checkout lives at
`~/Documents/code/TorrentNG`. If it lives somewhere else, edit
`WorkingDirectory` and `ExecStart` in
`~/.config/systemd/user/torrentng-live-main-update.service`.

Run one update immediately:

```sh
systemctl --user start torrentng-live-main-update.service
```

Watch the updater logs:

```sh
journalctl --user -u torrentng-live-main-update.service -f
```

The updater fetches `origin/main`, fast-forwards the checkout, rebuilds
`torrentng`, then recreates the service. The sample unit targets the local
certification stack on `http://localhost:28081`, using
`deploy/certification/compose.yml` and `deploy/certification/.env`, so the same
home test instance used by Sonarr/Radarr/Prowlarr/autobrr is refreshed from
`main`. Set `TNG_LIVE_COMPOSE_FILE`, `TNG_LIVE_COMPOSE_ENV_FILE`,
`TNG_HOST_PORT`, and `TNG_INCOMING_PORT` in the unit if your local instance
uses different compose wiring or ports. It refuses to run when the checkout has
uncommitted local changes so a test instance does not silently discard work. Use
a clean checkout for the live instance, or set `TNG_LIVE_ALLOW_DIRTY=1` only for
a disposable checkout.

Useful overrides:

| Variable | Default | Purpose |
|---|---|---|
| `TNG_LIVE_BRANCH` | `main` | Branch to follow |
| `TNG_LIVE_COMPOSE_FILE` | `deploy/docker/compose.yml` | Compose file to rebuild |
| `TNG_LIVE_SERVICE` | `torrentng` | Compose service to rebuild and recreate |
| `TNG_LIVE_FORCE` | `0` | Rebuild even when the commit did not change |
| `TNG_LIVE_PRUNE` | `0` | Run `docker image prune -f` after a successful update |
| `TNG_LIVE_DRY_RUN` | `0` | Print the commands without running them |

## Certification stack

The integration certification stack starts TorrentNG with Sonarr, Radarr, Prowlarr, autobrr, and cross-seed:

```sh
cp deploy/certification/.env.example deploy/certification/.env
CERT_START_STACK=1 ./scripts/live_certification.sh
```

The runner writes a markdown report under `certification/reports/`. Use it as the release gate for local qBittorrent API compatibility and container-level integration readiness, then complete the first-run app configuration in each service UI for full end-to-end add-torrent jobs.

The running engine can be audited through the native API:

```sh
curl -H "Authorization: Bearer $TNG_API_TOKEN" http://localhost:28080/api/v1/engine
curl -H "Authorization: Bearer $TNG_API_TOKEN" http://localhost:28080/api/v1/engine/commands
```

`/api/v1/engine` reports packaged/live rTorrent versions, bundled patch provenance, rTorrent HTTP tracker-stack settings, available XMLRPC capabilities, and drift from `engine-profile/rtorrent.rc`.
The drift gate intentionally checks only settings with stable readback commands in rTorrent `0.16.11`; set-only commands such as `protocol.encryption.set` and `dht.mode.set` remain covered by the packaged profile and source-controlled config.

For VPN-backed public DHT/peer reachability, TorrentNG can consume the same
forwarded-port state contract used by the slskdN VPN agent. The adapter reads
`/var/lib/slskdN-vpn/pf*.env`, `/etc/slskdN-vpn/static-forwards/pf*.env`, or a
Gluetun-compatible API and restarts the certification service with the matching
incoming port:

```sh
scripts/vpn/tng_forward_from_vpn_state.sh print
scripts/vpn/tng_forward_from_vpn_state.sh restart-cert
TNG_VPN_WATCH_INTERVAL=30 TNG_VPN_MISS_LIMIT=3 TNG_VPN_ON_MISSING=mark scripts/vpn/tng_vpn_forward_watch.sh
TNG_VPN_PUBLIC_PORT=50000 TNG_VPN_PUBLIC_IP=203.0.113.10 ./scripts/dht_certification.sh
```

The watcher keeps trying until a forward appears. When the public/private port
mapping changes, it rewrites `certification/reports/tng-vpn-forward.env`,
restarts the TorrentNG certification service with the current forwarded port,
and runs DHT certification. If no forward is present, it writes degraded state
and keeps polling. Set `TNG_VPN_ON_MISSING=stop-cert` plus
`TNG_VPN_MISS_LIMIT=N` to stop the certification service after repeated misses.

The Docker entrypoints pin `network.port_range`, `dht.port`, and
`dht.override_port` to `RTORRENT_INCOMING_PORT`, so the TCP peer listener and UDP
DHT listener use the same forwarded public port.

Security checks can be run against any sidecar config:

```sh
TNG_SECRET_KEY="$(openssl rand -hex 32)" TNG_API_TOKENS="token-one,token-two" ./scripts/security_review.sh deploy/docker/sidecar.config.toml
```

## Tagged Releases

Release builds are intentionally tag-only. Pushing commits to `main` does not
build or publish a release. To publish, create and push a `v*` tag that points
at a commit already on `main`; the release workflow verifies the tag ancestry,
builds the sidecar, WebUI, Docker images, creates or updates the GitHub Release,
and posts a Discord announcement.

Configure the Discord announcement webhook as the GitHub Actions secret
`DISCORD_RELEASE_WEBHOOK`. Do not commit webhook URLs to the repository.

## systemd install

The systemd examples run rTorrent and the sidecar as the `rtorrent` user and communicate over `/run/rtorrent/rpc.sock`.

Create the service user:

```sh
sudo useradd --system --home /var/lib/rtorrent --shell /usr/sbin/nologin rtorrent
```

Install directories:

```sh
sudo install -D -m 0644 deploy/systemd/torrentng.tmpfiles.conf /etc/tmpfiles.d/torrentng.conf
sudo systemd-tmpfiles --create /etc/tmpfiles.d/torrentng.conf
```

Install rTorrent config:

```sh
sudo install -D -m 0640 deploy/systemd/rtorrent.rc /etc/rtorrent/rtorrent.rc
sudo install -D -m 0640 engine-profile/rtorrent.rc /etc/rtorrent/profile.rc
```

Install sidecar config:

```sh
sudo install -D -m 0640 deploy/systemd/torrentng.config.toml /etc/torrentng/config.toml
```

Install units:

```sh
sudo install -D -m 0644 deploy/systemd/rtorrent.service /etc/systemd/system/rtorrent.service
sudo install -D -m 0644 deploy/systemd/torrentng-sidecar.service /etc/systemd/system/torrentng-sidecar.service
sudo systemctl daemon-reload
sudo systemctl enable --now rtorrent.service torrentng-sidecar.service
```

The sidecar unit sets `TNG_STATIC_DIR=/usr/share/torrentng/webui`; install built WebUI assets there for host deployments.
