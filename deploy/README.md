# deploy/

Deployment assets for TorrentNG's universal client interface: the TorrentNG
Engine and compatible-client WebUI/API service. Pick the section below that
matches the client ownership model you're running — see
[docs/ENGINE_REWRITE.md](../docs/ENGINE_REWRITE.md) if you haven't decided
yet.

## TorrentNG client (`torrentngd`) — first-party path

| Path | What it is |
|---|---|
| `native/compose.yml` | Docker Compose for the TorrentNG client (`torrentngd` + WebUI) |
| `native/Dockerfile` | TorrentNG client image |
| `native/config.toml` | Reference TorrentNG-client config (container-shaped paths) |
| `native/kubernetes/` | Namespace, Secret, Service, StatefulSet, Kustomization for `torrentngd` |
| `native/systemd/` | `torrentngd.service`, `sysusers.conf`, `tmpfiles.conf` for bare-metal TorrentNG-client deployments |
| `native/grafana/` | Dashboards and provisioning for the TorrentNG-client metrics stack |
| `native/prometheus.yml`, `native/torrentngd.rules.yml` | Prometheus scrape config and alert rules |

```sh
docker compose -f deploy/native/compose.yml up --build
docker compose -f deploy/native/compose.yml --profile observability up --build  # + Prometheus/Grafana
```

See [docs/NATIVE_DEPLOYMENT.md](../docs/NATIVE_DEPLOYMENT.md).

## Compatible-client WebUI/API service — rTorrent integration

| Path | What it is |
|---|---|
| `docker/compose.yml` | Default rTorrent + `torrentng` compatible-client service stack |
| `docker/compose.qbittorrent.yml` | Optional qBittorrent + compatible-client profile; requires backend credentials |
| `docker/compose.transmission.yml` | Optional Transmission + compatible-client profile; requires backend credentials |
| `docker/compose.deluge.yml` | Optional Deluge + compatible-client profile; requires backend credentials |
| `docker/compose.phase1.yml` | Lower-level Phase 1 rTorrent/ruTorrent bundle (profile testing only) |
| `docker/Dockerfile*`, `docker/entrypoint*.sh` | Images and entrypoints for both of the above |
| `docker/config/`, `docker/patches/` | rTorrent/ruTorrent packaging config and patches |
| `systemd/` | `rtorrent.service`, `torrentng-sidecar.service`, timers, VPN-forward watch unit, and prod config examples for bare-metal compatible-client deployments |

```sh
docker compose -f deploy/docker/compose.yml up --build          # rTorrent + compatible-client service
docker compose -f deploy/docker/compose.phase1.yml up --build   # Phase 1 rTorrent/ruTorrent only
```

The optional external-client profiles use a separate Compose overlay so their
required credentials do not block the default stack. Their management UIs are
bound to loopback; the compatible-client HTTP/API ports and HTTP-only Nginx
front door are also loopback-only by default. Use an authenticated TLS
reverse proxy for remote UI/API access. Torrent peer ports remain published
for inbound peers. See the profile setup instructions in
[docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md).

See [docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md) and
[engine-profile/](../engine-profile/) for the pinned rTorrent build these
stacks consume.

## Unraid

| Path | What it is |
|---|---|
| `../templates/torrentng-webui.xml` | Community Applications template: WebUI/API replacement for an rTorrent/qBittorrent/Transmission/Deluge install you already run |
| `../templates/torrentng.xml` | Community Applications template: full `torrentngd` stack, no external client |
| `unraid/README.md` | How to install directly from these template URLs today, and the Community Applications submission layout |

## Shared / cross-cutting

| Path | What it is |
|---|---|
| `nginx/nginx.conf` | Reverse-proxy front door used by `docker/compose.yml` (compatible-client service/WebUI) |
| `nginx/nginx.phase1.conf` | Matching reverse-proxy example for the Phase 1 rTorrent/ruTorrent bundle (not wired into `compose.phase1.yml` by default) |
| `certification/` | Integration certification stack — see its own [README](certification/README.md) |
| `interop/` | Docker Compose fixture for `scripts/interop_matrix.sh` (torrentngd, rTorrent, public-torrent fixtures, nginx) |

## Reserved, currently empty

`kubernetes/` and `compose/` at this level are placeholders and hold nothing
yet — the working Kubernetes manifests are under `native/kubernetes/`, and
both backend arrangements currently ship their Compose files directly in
`native/` and `docker/` rather than a shared `compose/`. If a generic
compatible-client Kubernetes manifest set gets added later, it belongs here.
