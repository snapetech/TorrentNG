# rt-migrate

Import planning and native DB apply support for rTorrent, qBittorrent, and
Transmission state.

## Status

Dry-run migration scanning and native DB apply plumbing are implemented for
rTorrent session directories, qBittorrent `BT_backup` directories, and
Transmission session directories.

The scanner is read-only. It discovers `.torrent` files, pairs fast-resume
sidecars by info-hash or file stem, extracts save paths, categories, tags,
labels, uploaded/downloaded counters, completion flags, and trackers, then
returns an auditable `MigrationPlan` or markdown report. The apply path writes
native torrent rows, file rows, tracker rows, labels, categories, transfer
counters, ratios, and completion state through `rt-db`.

Operator-facing migration and rollback guidance is documented in
[../../docs/MIGRATION.md](../../docs/MIGRATION.md) and
[../../docs/BACKUP_RESTORE.md](../../docs/BACKUP_RESTORE.md).

Run focused tests:

```sh
cargo test -p rt-migrate
```
