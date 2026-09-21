# Backup And Restore

This procedure covers TorrentNG-client state: SQLite session DB, stored torrent
metainfo, fastresume state, config, and operator-owned certification artifacts.

## What To Back Up

- TorrentNG-client `config.toml` and any environment file that provides
  `TORRENTNGD_CONFIG`, API tokens, storage roots, and listen ports.
- The TorrentNG-client session directory containing `torrentng.db`, stored `.torrent`
  blobs, fastresume state, and job/event state.
- Storage-root metadata if deployed separately from payload files.
- Certification reports under `certification/reports/` for release evidence.

Payload files do not need to be copied for a client-state backup if they are on
durable storage and paths will be restored unchanged. Back them up separately
according to the storage platform policy.

## Online Backup

Use SQLite's online backup support or stop writes briefly before copying. Do not
copy a hot WAL database by copying only the main `.db` file.

```sh
sqlite3 /config/session/torrentng.db ".backup '/backup/torrentng.db'"
rsync -a /config/session/torrents/ /backup/torrents/
rsync -a /config/session/fastresume/ /backup/fastresume/
cp /config/config.toml /backup/config.toml
```

Run the disposable repository certification drill after a release build. It
uses temporary session/data roots, performs a SQLite online backup, restores to
a different root, starts a second daemon, and proves that restored metadata can
be mutated. It does not copy payload bytes and never touches the configured
TorrentNG/LVM roots:

```sh
cargo build --release --locked -p torrentngd
scripts/backup_restore_certification.sh
```

The generated report is written under `certification/reports/`. Treat a
passing drill as proof of session-backup choreography only; payload backup,
filesystem snapshots, encryption, retention, and restore testing on the actual
storage device remain deployment responsibilities.

## Compatible-client sidecar state

The compatible-client sidecar stores `cache.db` and `peer_id_suffix` under its
configured `data_dir` (the packaged Compose default is `/var/lib/torrentng`).
Keep that directory with the service's configuration and rTorrent session
state. The Docker Compose files use separate named state volumes for each
backend service; do not share one state volume between simultaneously running
services.

Older Compose deployments did not mount `/var/lib/torrentng`, so their state
lived in the container's writable layer. Before the first update that adds the
named volume, stop the old service and seed the new volume before starting its
replacement. Run these commands with the same Compose project name and
environment used by the deployment:

```sh
service=torrentng
volume_key=torrentng-state
container_id="$(docker compose -f deploy/docker/compose.yml ps -q "$service")"
test -n "$container_id"
state_export="$(mktemp -d)"
docker compose -f deploy/docker/compose.yml stop "$service"
docker cp "$container_id:/var/lib/torrentng/." "$state_export/"
state_volume="$(docker compose -f deploy/docker/compose.yml config --format json | jq -r --arg key "$volume_key" '.volumes[$key].name')"
if docker volume inspect "$state_volume" >/dev/null 2>&1; then
  printf 'Refusing to overwrite existing volume %s\n' "$state_volume" >&2
  exit 1
fi
docker volume create "$state_volume"
docker run --rm \
  --mount "type=volume,source=$state_volume,target=/state" \
  --mount "type=bind,source=$state_export,target=/source,readonly" \
  alpine:3.20 sh -ec 'cp -a /source/. /state/'
docker compose -f deploy/docker/compose.yml up -d --build --force-recreate "$service"
printf 'Keep the state export at %s until restore is verified.\n' "$state_export"
```

For the qBittorrent, Transmission, or Deluge profiles, set `service` to
`torrentng-qbittorrent`, `torrentng-transmission`, or `torrentng-deluge`, and
set `volume_key` to the corresponding `torrentng-*-state` key in
`deploy/docker/compose.yml`. Keep the export until the service is healthy and
the persisted identity is verified; it contains private application state.

## Restore

1. Stop `torrentngd`.
2. Move the current session directory aside.
3. Restore `torrentng.db`, torrent blobs, fastresume state, and config.
4. Start `torrentngd`.
5. Check `/health`, `/api/v1/torrents`, and the qBit compatibility list.
6. Run targeted rechecks only for torrents whose payload paths changed.

## Migration Rollback

Migration scanners are read-only against source clients. Keep the original
rTorrent session directory, qBittorrent `BT_backup`, or Transmission session
directory unchanged until the TorrentNG client has seeded successfully.

If import output is wrong:

1. Stop `torrentngd`.
2. Restore the pre-import TorrentNG-client DB backup.
3. Adjust path/category/tag remaps or source staging files.
4. Re-run dry-run import and compare the markdown report before applying.

Do not delete the old client state until the TorrentNG client has passed a
restart, list, tracker, and sample recheck certification cycle.
