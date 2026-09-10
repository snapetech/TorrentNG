# deploy/

Deployment assets for both engine tracks. Pick the section below that matches
the engine you're running — see
[docs/ENGINE_REWRITE.md](../docs/ENGINE_REWRITE.md) if you haven't decided
yet.

## Native engine (`torrentngd`) — primary path

| Path | What it is |
|---|---|
| `native/compose.yml` | Docker Compose for the native stack (`torrentngd` + WebUI) |
| `native/Dockerfile` | Native daemon image |
| `native/config.toml` | Reference native config (container-shaped paths) |
| `native/kubernetes/` | Namespace, Secret, Service, StatefulSet, Kustomization for native `torrentngd` |
| `native/systemd/` | `torrentngd.service`, `sysusers.conf`, `tmpfiles.conf` for bare-metal native deploys |
| `native/grafana/` | Dashboards and provisioning for the native metrics stack |
| `native/prometheus.yml`, `native/torrentngd.rules.yml` | Prometheus scrape config and alert rules |

```sh
docker compose -f deploy/native/compose.yml up --build
docker compose -f deploy/native/compose.yml --profile observability up --build  # + Prometheus/Grafana
```

See [docs/NATIVE_DEPLOYMENT.md](../docs/NATIVE_DEPLOYMENT.md).

## rTorrent sidecar (Track 1) — migration/compatibility bridge

| Path | What it is |
|---|---|
| `docker/compose.yml` | rTorrent + `torrentng` sidecar stack |
| `docker/compose.phase1.yml` | Lower-level Phase 1 rTorrent/ruTorrent bundle (profile testing only) |
| `docker/Dockerfile*`, `docker/entrypoint*.sh` | Images and entrypoints for both of the above |
| `docker/config/`, `docker/patches/` | rTorrent/ruTorrent packaging config and patches |
| `systemd/` | `rtorrent.service`, `torrentng-sidecar.service`, timers, VPN-forward watch unit, and prod config examples for bare-metal Track 1 deploys |

```sh
docker compose -f deploy/docker/compose.yml up --build          # rTorrent + sidecar
docker compose -f deploy/docker/compose.phase1.yml up --build   # Phase 1 rTorrent/ruTorrent only
```

See [docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md) and
[engine-profile/](../engine-profile/) for the pinned rTorrent build these
stacks consume.

## Shared / cross-cutting

| Path | What it is |
|---|---|
| `nginx/nginx.conf` | Reverse-proxy front door used by `docker/compose.yml` (sidecar/WebUI) |
| `nginx/nginx.phase1.conf` | Matching reverse-proxy example for the Phase 1 rTorrent/ruTorrent bundle (not wired into `compose.phase1.yml` by default) |
| `certification/` | Integration certification stack — see its own [README](certification/README.md) |
| `interop/` | Docker Compose fixture for `scripts/interop_matrix.sh` (torrentngd, rTorrent, public-torrent fixtures, nginx) |

## Reserved, currently empty

`kubernetes/` and `compose/` at this level are placeholders and hold nothing
yet — the working Kubernetes manifests are under `native/kubernetes/`, and
both engines currently ship their Compose files directly in `native/` and
`docker/` rather than a shared `compose/`. If a generic sidecar Kubernetes
manifest set gets added later, it belongs here.
