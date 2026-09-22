# Deployment

This document covers the compatible-client WebUI/API service deployment path,
including the packaged rTorrent integration. For the first-party TorrentNG
client (`torrentngd`), use the [TorrentNG client deployment guide](NATIVE_DEPLOYMENT.md). For
the product model and client comparison workflow, use
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
| `127.0.0.1:8080` | `8080/tcp` | ruTorrent (loopback only by default) |
| `50000` | `50000/tcp` | BitTorrent incoming TCP |
| `50000` | `50000/udp` | BitTorrent incoming UDP |

For Phase 1, `PHASE1_INCOMING_PORT` selects the host peer port and
`PHASE1_CONTAINER_INCOMING_PORT` selects rTorrent's container listen port;
Compose maps the former to the latter for both TCP and UDP.

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

The container imports `/etc/rtorrent/rtorrent.rc`, which imports `engine-profile/rtorrent.rc`, then imports `/run/rtorrent/user.rc` when a read-only config overlay exists.

This bundle does not configure HTTP authentication for ruTorrent. Compose binds
its WebUI to loopback by default; only set `PHASE1_HTTP_BIND` to a remotely
reachable host address when an authenticated reverse proxy or equivalent
access control protects it. The BitTorrent peer ports remain published.

## Diagnostics

From the host:

```sh
./scripts/healthcheck.sh /run/rtorrent/rpc.sock http://localhost:8080 http://localhost:8080/rutorrent/
```

From inside the Phase 1 container:

```sh
/scripts/healthcheck.sh /run/rtorrent/rpc.sock http://localhost:8080 http://localhost/rutorrent/
```

## Compatible-client service config

`deploy/docker/sidecar.config.toml` is the container-oriented compatible-client
service config. It points at the Phase 1 rTorrent socket path and `/data`
storage root; the filename remains historical for deployment compatibility.

For a host install, copy the same shape to:

```text
~/.config/torrentng/config.toml
```

## Compatible-client service container

The main Dockerfile builds the Rust compatible-client service and React WebUI,
starts rTorrent, and serves the WebUI from the `torrentng` service process.

```sh
docker compose -f deploy/docker/compose.yml up --build
```

The `/config` bind is read-only. If `/config/config.toml` exists, the service
uses it; otherwise it uses the packaged
`deploy/docker/sidecar.config.toml` without writing into the bind mount. Set
`TNG_SECRET_KEY` and `TNG_API_TOKENS` in the compose environment for
production auth; `TNG_API_TOKENS` is a comma-separated list for automation
clients. Override `TNG_STATIC_DIR` only if you mount WebUI assets somewhere
other than `/usr/share/torrentng/webui`.

The default rTorrent settings UI writes to the persistent
`/var/lib/torrentng/rtorrent-ui-overlay.rc` file, not the read-only `/config`
bind. The entrypoint creates it owner-only on first start and imports it after
the copied user config. If `TNG_RTORRENT_OVERLAY` is customized, the path must
be absolute, use only letters, digits, `_`, `.`, `/`, or `-`, and reside on a
writable persistent mount; the entrypoint imports that configured path.

The service's HTTP/API host port (`8080`) and the bundled HTTP-only Nginx
front door (`80`) bind to `127.0.0.1` by default. Optional adapter ports
`8082`–`8084` do the same. These listeners do not provide TLS: use an
authenticated TLS reverse proxy for remote access, and do not override the
loopback binds without equivalent transport and access protection. Incoming
BitTorrent peer ports remain published separately. Compatibility session
cookies carry `Secure` by default; disable `auth.secure_cookies` only for an
explicitly trusted loopback HTTP setup.

Both compatible-client images run without root privileges. `PUID` and `PGID`
select the runtime and initial named-volume ownership (default `1000:1000`);
set them in the Compose environment before building. The same values are
passed to the LinuxServer backend containers so they can share downloads.

For a volume created by an older root-running image, stop the TorrentNG
service and every other process using shared downloads, make a backup, then
migrate the named volumes once with the matching Compose files and `.env`:

```sh
docker compose -f deploy/docker/compose.yml run --rm --no-deps \
  --user 0:0 --entrypoint /bin/sh torrentng \
  -ec 'chown -R "$PUID:$PGID" /data /session /var/lib/torrentng'
```

For Phase 1, use the same procedure with
`-f deploy/docker/compose.phase1.yml`, service `torrentng-phase1`, and
`/data /session`. For a host bind mount, perform the equivalent ownership
change on the exact configured storage root while all consumers are stopped;
do not recursively change a shared mount until its target UID/GID and backup
are confirmed. Read-only `/config` files only need to be readable by the
selected UID/GID.

Compose stores `/var/lib/torrentng` in a dedicated named volume for each
compatible-client service. It contains the sidecar SQLite cache and the
persisted per-install peer-ID suffix; keep the matching volume with the
service's `/config` and `/session` state. For a pre-existing deployment that
has no `/var/lib/torrentng` mount, export and seed this directory before
recreating the container. The exact migration and backup steps are in
[BACKUP_RESTORE.md](BACKUP_RESTORE.md).

The compatible-client and Phase 1 images now run as the configured nonzero
UID/GID, reject UID 0 at entrypoint, keep `/config` read-only, and use
unprivileged container ports. Compose also drops all Linux capabilities and
sets `no-new-privileges`; Docker's default seccomp profile remains enabled.
Existing data volumes need the one-time ownership migration above before the
new image can write them.

### Optional qBittorrent, Transmission, and Deluge backends

The backend profiles are split into `compose.qbittorrent.yml`,
`compose.transmission.yml`, and `compose.deluge.yml` overlays. This keeps their
required credentials from blocking the default rTorrent stack. Set the
matching variables in a protected, untracked `.env` file alongside
`TNG_SECRET_KEY` and `TNG_API_TOKENS`; Compose fails closed when a selected
profile's adapter credentials are missing.

Backend WebUI/RPC host ports and the compatible-client adapter host ports are
loopback-only by default. Torrent peer ports remain published. Transmission's profile wires its credentials through the
image's `USER` and `PASS` variables. The qBittorrent image generates a
temporary password on first start; change it in the local WebUI to match the
configured `QBITTORRENT_USERNAME` and `QBITTORRENT_PASSWORD` before starting
the TorrentNG adapter. Deluge's password is stored by Deluge rather than
configured through the image environment; change it in its local WebUI to
match `DELUGE_PASSWORD`.

Example for qBittorrent:

```sh
docker compose --env-file .env \
  -f deploy/docker/compose.yml \
  -f deploy/docker/compose.qbittorrent.yml \
  --profile qbittorrent up -d qbittorrent
```

After setting the backend credentials, start both the backend and its adapter:

```sh
docker compose --env-file .env \
  -f deploy/docker/compose.yml \
  -f deploy/docker/compose.qbittorrent.yml \
  --profile qbittorrent up -d qbittorrent torrentng-qbittorrent
```

Use the corresponding overlay and profile name for Transmission or Deluge.

To front an rTorrent instance you already run elsewhere instead of one of
these bundled overlays, set `TNG_BACKEND=rtorrent`, `TNG_RTORRENT_MANAGED=0`,
and `TNG_SCGI_ADDR` (or `TNG_SCGI_SOCKET`) to that instance -- the same knobs
the Unraid `torrentng-webui` template uses (see
[deploy/unraid/README.md](../deploy/unraid/README.md)).

### Home live-main updater

For a home test instance that should follow GitHub `main`, run the updater from
the host instead of trying to mutate the running container. The Docker image must
still be rebuilt because it contains the compiled `torrentng` service, built WebUI
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

The running TorrentNG client can be audited through the TorrentNG API:

```sh
curl -H "Authorization: Bearer $TNG_API_TOKEN" http://localhost:28080/api/v1/engine
curl -H "Authorization: Bearer $TNG_API_TOKEN" http://localhost:28080/api/v1/engine/commands
```

`/api/v1/engine` reports packaged/live rTorrent versions, bundled patch provenance, rTorrent HTTP tracker-stack settings, available XMLRPC capabilities, and drift from `engine-profile/rtorrent.rc`.
The drift gate intentionally checks only settings with stable readback commands in rTorrent `0.16.11`; set-only commands such as `protocol.encryption.set` and `dht.mode.set` remain covered by the packaged profile and source-controlled config.

The Docker image declares the packaged rTorrent patches in
`TNG_RTORRENT_PATCHES`. Production-like deployments should verify
`d.multicall.range` and `tng.live_summary` through `/api/v1/engine/commands`
or startup logs before relying on large-list compatible-client synchronization.
The service must keep rTorrent's leading XMLRPC target argument on patched
calls; for global calls that argument is the empty string, followed by the
view/range parameters.
Without it, rTorrent returns `invalid target`, torrent-list sync fails, and the
WebUI reports the backend as disconnected.

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

Security checks can be run against any compatible-client service config:

```sh
TNG_SECRET_KEY="$(openssl rand -hex 32)" TNG_API_TOKENS="token-one,token-two" ./scripts/security_review.sh deploy/docker/sidecar.config.toml
```

## Tagged Releases

Release builds are intentionally tag-only. Pushing commits to `main` does not
build or publish a release. To publish, create and push a `main-*` tag that
points at a commit already on `main`; the release workflow verifies the tag
ancestry, builds the compatible-client service, WebUI, and Docker images,
creates or updates the
GitHub Release, and posts a Discord announcement.

Configure the Discord announcement webhook as the GitHub Actions secret
`DISCORD_RELEASE_WEBHOOK`. Do not commit webhook URLs to the repository.

## systemd install

The systemd examples run rTorrent and the compatible-client service as the
`rtorrent` user and communicate over `/run/rtorrent/rpc.sock`.

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

Install compatible-client service config:

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

The compatible-client service unit sets
`TNG_STATIC_DIR=/usr/share/torrentng/webui`; install built WebUI assets there
for host deployments.
