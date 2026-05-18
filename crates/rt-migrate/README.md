# rt-migrate

Import planning, native DB apply support, fast-resume state import, and
reverse export for major BitTorrent clients.

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
uTorrent/BitTorrent classic. The per-torrent uTorrent entry's `have`/`bitfield`
byte string is decoded to per-piece state, so completed pieces import as
`Valid` and skip the full recheck under the default `TrustHints` policy, the
same as qBittorrent/Deluge. rTorrent complete-state sidecars can synthesize
seed piece state when matching files are present, and BiglyBT/Vuze
`downloads.config` style aggregate state is matched by info-hash and imports
nested resume bitfields when present.

Tixati is scannable for `.torrent` metadata, but its progress/resume state is
an undocumented proprietary binary format (not bencode/JSON). It is kept
verification-first by design: an opaque or corrupt Tixati sidecar must never
be guessed into trusted piece state (that would risk seeding corrupt data), so
it falls back to a normal recheck. Lifting this requires a real Tixati state
corpus and a strict, validated decoder, not a heuristic — tracked as a known
gap, not a silent one.

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

## Reverse export

`rt_migrate::export` is the anti-lock-in path. It reads native state
read-only from the session DB, persisted `.torrent` blobs, and native
fastresume files, then writes target-client layouts:

- `ExportFormat::Libtorrent` for qBittorrent/Deluge style `.fastresume`
- `ExportFormat::Transmission` for `torrents/` plus `resume/`
- `ExportFormat::Rtorrent` for session `.torrent` plus complete-state sidecars
- `ExportFormat::Utorrent` for aggregate `resume.dat`
- `ExportFormat::Biglybt` for aggregate `downloads.config`
- `ExportFormat::Generic` for `.torrent` files plus a JSON manifest

The export plan reports fidelity as recheck-free, complete-only,
metadata-only, or torrent-only. Libtorrent, Transmission, uTorrent, and
BiglyBT exports carry piece maps when native fastresume exists. rTorrent can
only avoid recheck for complete torrents. Generic export is always correct but
expects the destination client to recheck.

Run focused tests:

```sh
cargo test -p rt-migrate
```
