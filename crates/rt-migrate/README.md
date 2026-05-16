# rt-migrate

import from rTorrent, qBittorrent, Transmission.

## Status

Dry-run migration scanning is implemented for rTorrent session directories,
qBittorrent `BT_backup` directories, and Transmission session directories.

The scanner is read-only. It discovers `.torrent` files, pairs fast-resume
sidecars by info-hash or file stem, extracts save paths, categories, tags,
labels, uploaded/downloaded counters, completion flags, and trackers, then
returns an auditable `MigrationPlan` or markdown report.

Apply/import into the native database is still pending.
