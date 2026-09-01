# Native Engine Deployment

Native mode runs `torrentngd` as the source of truth. qBittorrent,
Transmission, Deluge, and legacy UI compatibility surfaces are facades over the
same durable engine state.

For the larger rewrite overview and native-vs-rTorrent comparison, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

## Production Requirements

- Put the session DB and torrent metadata on durable local storage.
- Put payload data on mounted storage roots with stable paths.
- Set native API tokens in `[auth].api_tokens`; public binds reject missing,
  short, or placeholder tokens at startup.
- Bind the native API behind TLS or a trusted reverse proxy.
- Keep mutating endpoints token-protected.
- Enable scripts only with a root-owned allowlist directory.
- Run backup before imports, bulk moves, or upgrades.

## Config

`torrentngd` loads config from `TORRENTNGD_CONFIG`, then
`~/.config/torrentngd/config.toml`, then `/etc/torrentngd/config.toml`.
When no config exists, defaults are used.

Minimal production shape:

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/torrentngd"

[storage]
download_dir = "/data"

[auth]
api_tokens = ["REPLACE_WITH_A_RANDOM_TOKEN_OF_AT_LEAST_16_CHARACTERS"]
```

The daemon stores SQLite state at `session_dir/state.db` unless `[db].path` is
set explicitly. See [CONFIGURATION.md](CONFIGURATION.md) for the full native
config surface.

## Start

```sh
TORRENTNGD_CONFIG=/config/config.toml torrentngd
```

Minimum validation:

```sh
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8080/api/v1/torrents
curl -fsS http://127.0.0.1:8080/api/qb/v2/torrents/info
```

## Docker Compose

The native Compose stack builds `torrentngd`, mounts durable state and payload
volumes, and can optionally start Prometheus and Grafana:

```sh
docker compose -f deploy/native/compose.yml up --build
docker compose -f deploy/native/compose.yml --profile observability up --build
```

The example config is [deploy/native/config.toml](../deploy/native/config.toml).
Replace the token placeholder with a random value, then change storage paths and
the public peer port before using it outside local testing. The shipped example
intentionally refuses to start until the placeholder is replaced.

## systemd

Example unit and tmpfiles definitions are in [deploy/native/systemd](../deploy/native/systemd).
Install the binary and config, create the service user, then enable the unit:

```sh
install -Dm755 target/release/torrentngd /usr/local/bin/torrentngd
install -Dm644 deploy/native/config.toml /etc/torrentngd/config.toml
install -Dm644 deploy/native/systemd/torrentngd.service /etc/systemd/system/torrentngd.service
install -Dm644 deploy/native/systemd/sysusers.conf /etc/sysusers.d/torrentngd.conf
install -Dm644 deploy/native/systemd/tmpfiles.conf /etc/tmpfiles.d/torrentngd.conf
systemd-sysusers /etc/sysusers.d/torrentngd.conf
systemd-tmpfiles --create /etc/tmpfiles.d/torrentngd.conf
systemctl enable --now torrentngd
```

## Kubernetes

Kubernetes examples live under [deploy/native/kubernetes](../deploy/native/kubernetes):

```sh
kubectl apply -k deploy/native/kubernetes
```

The StatefulSet uses persistent volume claims for session state and downloads.
The config is mounted from a Secret because it contains API tokens. Adjust
storage classes, sizes, ingress/load-balancer exposure, and tokens for the
target cluster.

## Observability

`torrentngd` exposes Prometheus metrics at `/metrics`. The native deployment
directory includes:

- [prometheus.yml](../deploy/native/prometheus.yml)
- [Grafana datasource provisioning](../deploy/native/grafana/provisioning/datasources/prometheus.yml)
- [torrentngd overview dashboard](../deploy/native/grafana/dashboards/torrentngd.json)

With Compose, start them through the `observability` profile.

## Arch Package

An Arch/AUR packaging template is available under [packaging/arch](../packaging/arch).
Build locally with:

```sh
cd packaging/arch
makepkg -si
```

## Upgrade

1. Run `scripts/native_engine_certification_report.sh` on the current build.
2. Back up session state with [BACKUP_RESTORE.md](BACKUP_RESTORE.md).
3. Deploy the new binary/container.
4. Verify `/health`, native list, qBit list, and metrics.
5. Keep the previous binary/container image until restart recovery is confirmed.

## Certification

The native release gate is:

```sh
scripts/native_engine_certification_report.sh
```

For a fast deterministic preflight that does not need Docker or public swarm
access, run:

```sh
scripts/local_release_gate.sh
```

The local gate writes a markdown report under `certification/reports/` with
the branch, commit, worktree status, each command, per-gate duration, and the
aggregate pass/fail result. Set `TNG_STORAGE_MATRIX_TARGETS='/mnt/nvme
/mnt/hdd'` to run the full storage release certification wrapper in the same
report; that wrapper covers the hardware matrix, `io_uring` graduation,
real-root move/import, and storage certification index.

When a daemon is running, bind the certification report to the live `/health`
capability manifest as well:

```sh
NATIVE_ENGINE_URL=http://127.0.0.1:8080 scripts/native_engine_certification_report.sh
```

The post-soak release gate also reruns native engine rewrite certification
directly before refreshing the aggregate release report.

Live public transfer evidence is optional for offline CI but required before a
production release:

```sh
scripts/public_linux_iso_certification.sh
```
