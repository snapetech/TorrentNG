# rt-migrate

import from rTorrent, qBittorrent, Transmission.

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

Operator-facing CLI wiring and rollback/backup documentation are still pending.
