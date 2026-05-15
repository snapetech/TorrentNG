# Migration Guide

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

Map your existing session and data directories to the container volumes:

```yaml
# In your compose override:
volumes:
  - /your/existing/rtorrent-session:/session
  - /your/existing/downloads:/data
  - /your/existing/config:/config
```

Place any `.rtorrent.rc` overrides in `/config/rtorrent.rc`.

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

**Step 3: Run import (Phase 3+)**

The migration tool is not yet implemented. Track: `rt-migrate` crate in Track 2 Phase 0.

**Workaround until then:** Load each `.torrent` file manually via the rtorrentNG API or ruTorrent UI, pointing to the existing file path. rTorrent will verify and resume without downloading.

---

## Migrating from Transmission

### What gets imported

- Torrent files from `~/.config/transmission/torrents/`
- Resume data from `~/.config/transmission/resume/`
- Download directories

### Procedure

Transmission migration tooling is planned for Track 2 Phase 12. Until then:

1. Export torrent files from Transmission (right-click → "Export .torrent")
2. Add each via the rtorrentNG API or ruTorrent, pointing to the existing download path
3. rTorrent will verify pieces and resume seeding without re-downloading

---

## Path remapping

If your download directories are in different locations in the new setup, you need to remap paths before adding torrents.

```sh
# Example: old path /downloads, new path /data
# Set in rtorrent.rc or via API after adding:
# d.directory.set=<new_path>/<torrent_name>
```

The Phase 2 sidecar API (`PUT /api/v1/torrents/{hash}` with `save_path`) handles this per-torrent. A bulk path remap tool with dry-run preview is planned for Phase 4.

---

## Rolling back

If migration causes problems:

1. Stop the new container
2. Restore your session backup: `cp -r ~/rtorrent-session.bak ~/.rtorrent-session`
3. Start your old rTorrent install

No migration step modifies the session directory in-place. The import is read-only from the source.
