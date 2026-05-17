# Migration Guide

This guide covers moving existing client state into rtorrentNG. The migration
goal is the import side of universal compatibility: preserve the torrent
metadata, paths, labels/categories/tags, counters, file priorities, and resume
state that other clients have accumulated wherever the source format makes that
recoverable.

Current migration coverage includes rTorrent, qBittorrent, Transmission, Deluge,
uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati, and generic `.torrent`
directories, with exact status tracked in
[CLIENT_COMPATIBILITY_MATRICES.md](CLIENT_COMPATIBILITY_MATRICES.md). For the
broader native rewrite overview and engine swap workflow, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

## Migrating from rTorrent + ruTorrent

### What gets imported

- rTorrent session directory (`.torrent` files and `.rtorrent` resume files)
- Save paths from resume state
- Labels/categories stored in `d.custom1`
- Tracker lists
- Upload/download stats from resume state
- Completed/seeding state (no forced recheck for complete torrents)

### What does not import automatically

- ruTorrent per-plugin settings (RSS rules, ratio groups, labels UI state)
- ruTorrent views and saved searches
- Plugin-stored custom metadata beyond `d.custom1–5`

### Procedure

**Step 1: Run the diagnostic first**

```sh
./scripts/healthcheck.sh /path/to/rtorrent.sock
```

Fix any issues it reports before migrating.

**Step 2: Back up everything**

```sh
# rTorrent session
cp -r ~/.rtorrent-session ~/rtorrent-session.bak

# ruTorrent settings
cp -r /var/www/rutorrent/conf ~/rutorrent-conf.bak
cp -r /var/www/rutorrent/share ~/rutorrent-share.bak
```

**Step 3: Note your current config**

```sh
cat ~/.rtorrent.rc
```

Key values to record:
- `session.path` — session directory
- `directory.default.set` — default download path
- `network.port_range.set` — listen port
- `scgi_local` or `scgi_port` — RPC socket

**Step 4: Deploy the Phase 1 bundle**

```sh
docker compose -f deploy/docker/compose.phase1.yml up --build
```

Map your existing session and data directories to the container volumes:

```yaml
# In your compose override:
volumes:
  - /your/existing/rtorrent-session:/session
  - /your/existing/downloads:/data
  - /your/existing/config:/config
```

Place any `.rtorrent.rc` overrides in `/config/rtorrent.rc`.

More deployment details are in [DEPLOYMENT.md](DEPLOYMENT.md).

**Step 5: Verify**

After starting the new container:
1. Check the ruTorrent torrent list — all torrents should appear in their previous state
2. Verify no torrents have started unnecessary rechecks
3. Check that tracker announces are working
4. Confirm save paths are correct

### Notes

- Complete torrents that were seeding will resume seeding. They are not forced to recheck.
- Partial downloads resume from where they left off (rTorrent resume state is preserved).
- If a torrent shows "hash check" on startup, the resume file may be missing or corrupt — this is from rTorrent's own resume logic, not a migration artifact.

---

## Migrating from qBittorrent

### What gets imported

- `BT_backup/` directory (`.torrent` files and `.fastresume` files)
- Categories
- Tags
- Save paths
- Torrent states (paused/seeding/downloading)
- Tracker lists

### Procedure

**Step 1: Locate qBittorrent data**

Common locations:
- Linux: `~/.local/share/data/qBittorrent/BT_backup/`
- Docker: inside the container at the above path

**Step 2: Copy BT_backup to a staging directory**

```sh
cp -r ~/.local/share/data/qBittorrent/BT_backup /tmp/qbt-migration
```

**Step 3: Run dry-run scan and native import**

The `rt-migrate` crate now provides qBittorrent `BT_backup` dry-run scanning and
native database apply plumbing. It pairs `.torrent` files with `.fastresume`
sidecars and imports save path, category, tags, trackers, completion state, file
rows, transfer counters, and ratio.

Operator-facing CLI wiring can use the native migration crate directly from
integration tooling. Manual fallback remains available: load each `.torrent`
through the rtorrentNG API, pointing at the existing file path, then let the
native recheck job verify and resume without downloading.

---

## Migrating from Transmission

### What gets imported

- Torrent files from `~/.config/transmission/torrents/`
- Resume data from `~/.config/transmission/resume/`
- Download directories

### Procedure

The native migration crate imports Transmission torrent and resume state into
the rtorrentNG session DB. Use a dry-run report first, back up the native DB,
then apply the import from integration tooling. Manual fallback remains:

1. Export torrent files from Transmission (right-click → "Export .torrent")
2. Add each via the rtorrentNG API, pointing to the existing download path
3. Run a native recheck job so verified pieces resume without re-downloading

---

## Migrating from Deluge, uTorrent, BitTorrent Classic, BiglyBT/Vuze, or Tixati

The native migration crate also scans common state directories from these
clients. It imports `.torrent` metadata, save paths, labels/categories, transfer
counters, lifecycle timestamps, active/paused state, per-file wanted/priority
state, per-file completed bytes, trackers, generic tracker timing/scrape state,
and any compatible fast-resume piece state it can decode.

Current fast-resume confidence:

- rTorrent: complete `.rtorrent` sidecars can synthesize seed piece state when
  matching files are present; incomplete or unsupported sidecars fall back to
  verification.
- Deluge: libtorrent-style resume data is imported like qBittorrent.
- uTorrent/BitTorrent classic: aggregate `resume.dat` entries keyed by raw,
  hex, or base32 info-hash are imported when matching `.torrent` files are
  present.
- BiglyBT/Vuze: aggregate `downloads.config` entries keyed by info-hash are
  imported, including nested resume bitfields when present.
- Tixati: metadata scanning is available; proprietary progress state remains
  verification-first until a strict decoder is added.

Use dry-run output before applying any import. The source directories are read
only. Dry-run reports include trusted, hints, metadata-only, and none counts so
operators can see how much state will avoid a full recheck. Low-confidence or
unsupported resume data is downgraded to normal verification rather than trusted
as complete. Piece-state length mismatches are reported and normalized to the
torrent piece count before native fast-resume state is written.

---

## Path remapping

If your download directories are in different locations in the new setup, you need to remap paths before adding torrents.

```sh
# Example: old path /downloads, new path /data
# Set in rtorrent.rc or via API after adding:
# d.directory.set=<new_path>/<torrent_name>
```

The native and sidecar API surfaces both support per-torrent save path updates.
Bulk moves should use the native storage planning flow so conflicts, capacity,
copy/rename mode, rollback, and destructive delete approval are visible before
data moves.

Native migration tooling can also apply path remaps during dry-run/import. A
remap such as `/downloads -> /data` is used both for file-hint validation, so
trusted fast-resume state can be recognized after moving into a container, and
for the native DB save path written during import.

For native engine imports, use the combined native import path so DB rows and
compatible fast-resume state are written together under one audited summary.

---

## Rolling back

If migration causes problems:

1. Stop the new container.
2. Restore the native DB/session backup created before import.
3. Keep the original source client session untouched and restart it if needed.
4. Re-run the dry-run report after correcting path/category/tag remaps.

No migration step modifies the session directory in-place. The import is read-only from the source.
See [BACKUP_RESTORE.md](BACKUP_RESTORE.md) for native DB backup and restore
commands.
