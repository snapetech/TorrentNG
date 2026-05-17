# rt-migrate

Import planning, native DB apply support, and fast-resume state import for
major BitTorrent clients.

## Status

Dry-run migration scanning and native DB apply plumbing are implemented for
rTorrent session directories, qBittorrent `BT_backup` directories, Transmission
session directories, Deluge state folders, uTorrent/BitTorrent classic config
folders, BiglyBT/Vuze config folders, Tixati config folders, and generic
directories of `.torrent` files.

The scanner is read-only. It discovers `.torrent` files, pairs fast-resume
sidecars by info-hash or file stem, extracts save paths, categories, tags,
labels, uploaded/downloaded counters, lifecycle timestamps, active/paused state,
completion flags, per-file wanted/priority state, per-file completed bytes,
trackers, and generic tracker activity, then returns an auditable
`MigrationPlan` or markdown report with a confidence summary. The apply path
writes native torrent rows, file rows, tracker rows, labels, categories,
transfer counters, ratios, completion state through `rt-db`, and compatible
fast-resume states through `rt-fastresume`.

Fast-resume import is confidence-rated:

- `Trusted`: piece state was decoded and matching files were found with expected
  sizes, so the generated `rt-fastresume` state can use `TrustHints`.
- `Hints`: piece state was decoded, but file hints could not fully validate it.
  Operators can still choose `TrustAll`, or keep the default verification path.
- `MetadataOnly`: labels, paths, stats, and completion metadata were found, but
  no compatible piece state was decoded.
- `None`: only `.torrent` metadata was importable.

The current trusted/hints decoders cover libtorrent-style resume data used by
qBittorrent and Deluge, Transmission progress bitfields, and aggregate
`resume.dat` dictionaries keyed by raw, hex, or base32 info-hash as used by
uTorrent/BitTorrent classic. rTorrent complete-state sidecars can synthesize
seed piece state when matching files are present, and BiglyBT/Vuze
`downloads.config` style aggregate state is matched by info-hash and imports
nested resume bitfields when present. Tixati is scannable now, with unsupported
or proprietary resume details kept metadata-only until a strict decoder is
added.

Decoded piece vectors are normalized to the torrent piece count with warnings
when imported state has to be truncated or padded. Partial-piece block lists are
sorted, deduplicated, and bounded before being written to native fast-resume
state.

`ImportOptions::path_remaps` can translate old client save roots to their new
host/container locations. Remaps are applied when collecting file hints during
dry-run scanning and when writing native DB save paths.

Use `MigrationPlan::apply_native_import` when callers want the normal native
migration path: it writes DB rows and compatible `rt-fastresume` state together
and returns both summaries.

Operator-facing migration and rollback guidance is documented in
[../../docs/MIGRATION.md](../../docs/MIGRATION.md) and
[../../docs/BACKUP_RESTORE.md](../../docs/BACKUP_RESTORE.md).

Run focused tests:

```sh
cargo test -p rt-migrate
```
