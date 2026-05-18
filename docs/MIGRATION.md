# Migration Guide

This guide covers moving existing client state into TorrentNG. The migration
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

## The `torrentngd migrate` command

For native-engine deployments, `torrentngd migrate` is the one-shot import
path. It scans a source client's state directory read-only, prints a dry-run
report, and (with `--apply`) writes native DB rows and compatible fast-resume
state together so complete torrents resume seeding without a full recheck.

```sh
torrentngd migrate --source <SRC> --from <DIR> [OPTIONS]
```

- `--source` — `rtorrent`, `qbittorrent`, `transmission`, `deluge`,
  `utorrent`, `biglybt`, `tixati`, or `generic`
- `--from` — the source client state directory (never modified)
- `--apply` — perform the import; **omit for a dry-run report** (the default)
- `--policy` — fast-resume trust: `verify`, `trust-hints` (default), or
  `trust-all`. `trust-hints` only trusts complete state when the data files
  are present at the expected sizes; anything else falls back to verification.
- `--remap OLD=NEW` — rewrite a save-path prefix (repeatable), e.g.
  `--remap /downloads=/data` when data moved into a container
- `--default-save-path DIR` — fallback save path when the source recorded none
- `--report FILE` — also write the markdown dry-run report to `FILE`
- `--config FILE` — config file (else `TORRENTNGD_CONFIG` / defaults); this
  determines the native DB and fast-resume target locations
- `--yes` — skip the confirmation prompt with `--apply`

The dry-run report and the post-apply summary both break torrents down into
trusted / hints / metadata-only / none so you can see how much state will
avoid a recheck before committing. Always run the dry-run first. The native
DB and fast-resume directory are taken from the resolved config, so point
`--config` at the same config the daemon uses, and back up the native DB (see
[BACKUP_RESTORE.md](BACKUP_RESTORE.md)) before `--apply`.

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

### Native engine import (recommended)

For native-engine deployments, import the rTorrent session directly with
[`torrentngd migrate`](#the-torrentngd-migrate-command). Stop rTorrent (or work
from a copy of the session directory) so resume files are not mid-write, then:

```sh
# 1. Dry-run: read-only, shows trusted/hints/metadata-only/none counts
torrentngd migrate --source rtorrent \
  --from ~/.rtorrent-session \
  --report /tmp/rtorrent-migration.md

# 2. Back up the native DB (see BACKUP_RESTORE.md), then apply
torrentngd migrate --source rtorrent \
  --from ~/.rtorrent-session \
  --remap /old/downloads=/data \
  --apply
```

Complete torrents whose data files are present at the expected sizes import as
`trusted` and resume seeding with no full recheck. Incomplete downloads and any
torrent whose files are missing or resized fall back to verification — rTorrent
partial resume state is not decoded, so in-progress downloads will recheck.
Use `--remap` if the download paths differ in the new deployment. The rTorrent
session directory is read only; nothing is written to it.

### Procedure (Track 1 rTorrent sidecar)

Use this path when keeping rTorrent as the engine (migration/comparison).

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

```sh
torrentngd migrate --source qbittorrent --from /tmp/qbt-migration --report /tmp/qbt.md
torrentngd migrate --source qbittorrent --from /tmp/qbt-migration --apply
```

This pairs `.torrent` files with `.fastresume` sidecars and imports save path,
category, tags, trackers, completion state, file rows, transfer counters, and
ratio. qBittorrent's libtorrent-style resume data carries piece state, so
complete and partial torrents both import their progress and avoid a full
recheck under the default `trust-hints` policy when the data files are present.

Manual fallback remains available: load each `.torrent` through the TorrentNG
API, pointing at the existing file path, then let the native recheck job verify
and resume without downloading.

---

## Migrating from Transmission

### What gets imported

- Torrent files from `~/.config/transmission/torrents/`
- Resume data from `~/.config/transmission/resume/`
- Download directories

### Procedure

Run [`torrentngd migrate`](#the-torrentngd-migrate-command) against the
Transmission config directory (the parent of `torrents/` and `resume/`):

```sh
torrentngd migrate --source transmission --from ~/.config/transmission --report /tmp/tr.md
torrentngd migrate --source transmission --from ~/.config/transmission --apply
```

Transmission progress bitfields are decoded, so completed and partial torrents
avoid a full recheck when their data is present. Manual fallback remains:

1. Export torrent files from Transmission (right-click → "Export .torrent")
2. Add each via the TorrentNG API, pointing to the existing download path
3. Run a native recheck job so verified pieces resume without re-downloading

---

## Migrating from Deluge, uTorrent, BitTorrent Classic, BiglyBT/Vuze, or Tixati

Pass the matching `--source` (`deluge`, `utorrent`, `biglybt`, or `tixati`)
to [`torrentngd migrate`](#the-torrentngd-migrate-command) with the client's
state/config directory as `--from`. The scanner imports `.torrent` metadata,
save paths, labels/categories, transfer
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

Pass remaps to `torrentngd migrate` with repeatable `--remap OLD=NEW` flags
(e.g. `--remap /downloads=/data`). Each remap is applied both to file-hint
validation — so trusted fast-resume state is still recognized after moving data
into a container — and to the native DB save path written during import. The
longest matching prefix wins when multiple remaps apply.

`torrentngd migrate --apply` is the combined native import path: DB rows and
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
