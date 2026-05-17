# Backup And Restore

This procedure covers native-engine state: SQLite session DB, stored torrent
metainfo, fastresume state, config, and operator-owned certification artifacts.

## What To Back Up

- Native `config.toml` and any environment file that provides
  `TORRENTNGD_CONFIG`, API tokens, storage roots, and listen ports.
- The native session directory containing `torrentng.db`, stored `.torrent`
  blobs, fastresume state, and job/event state.
- Storage-root metadata if deployed separately from payload files.
- Certification reports under `certification/reports/` for release evidence.

Payload files do not need to be copied for a session-state backup if they are on
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
directory unchanged until the native engine has seeded successfully.

If import output is wrong:

1. Stop `torrentngd`.
2. Restore the pre-import native DB backup.
3. Adjust path/category/tag remaps or source staging files.
4. Re-run dry-run import and compare the markdown report before applying.

Do not delete the old client state until the native engine has passed a restart,
list, tracker, and sample recheck certification cycle.
