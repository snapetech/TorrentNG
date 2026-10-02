# Crash Safety

How TorrentNG keeps a power cut, kernel panic or hard reset from turning a
half-written download into a "complete" torrent that Sonarr, Radarr or a
tracker then trusts.

This applies to the TorrentNG client (`torrentngd`). When TorrentNG fronts
another client (rTorrent, qBittorrent, Transmission, Deluge) that client owns
the transfer and its own resume state; see [Migration](#migration) for the one
place the two meet.

Every option below can be changed without a restart, from the WebUI
(**Settings → Backend → Crash safety**), the API, or the config file. Nothing
here requires a specific operating system to be *correct*; a few checks that
depend on OS features simply report `unsupported` and step aside. See the
[platform matrix](#platform-support).

## The failure this exists to prevent

Several torrent clients, rTorrent among them, do this:

1. Preallocate every file to full size when the torrent is added.
2. Write pieces into the page cache as they arrive and hash-verify them in
   memory.
3. Periodically save a resume record ("pieces 0–4711 are complete").
4. After a restart, trust the resume record and do not re-hash.

Step 3 can reach the disk **before** step 2's data does. If the machine loses
power in between, the file is still at full size (from step 1), the resume
record still claims the piece is complete, and the bytes inside it are zeros.
The client marks the file complete and hands it on untouched. Nothing is
checking, and nothing looks wrong: the size is right and the state says "done".

The root cause is ordering: **a claim was persisted before the data it
describes was durable.** Everything in this document is a way of enforcing
that ordering, detecting when it was violated anyway, or making the damage
visible and repairable.

## What TorrentNG guarantees, and what it does not

**Guaranteed** (on storage that honors `fsync`):

- A fastresume record never claims a piece is valid unless the payload data for
  it was fsynced before the record was written.
- A finished download is not reported complete, not announced `completed` to
  the tracker, and not stamped `completed_at` until its data is durable
  (the *completion gate*).
- After a host crash, a torrent whose pieces the filesystem says were never
  written is not trusted (the *allocation audit*).
- A torrent already reported complete that later fails verification is moved
  back to downloading, repaired **in place**, and reported.

**Not guaranteed** (nothing in userspace can):

- Storage that acknowledges an `fsync` it did not perform: disks with a
  volatile write cache that ignore flushes, RAID controllers without battery
  backup, some network and FUSE filesystems, ZFS datasets with `sync=disabled`.
  TorrentNG *classifies* mounts and can add extra checking on the risky ones
  ([storage trust](#storage-trust)), but it cannot see the drive's own cache.
- Bit rot, or data corrupted after it was durably written. Recheck finds that;
  this feature does not prevent it.
- Copies of a file made **before** a crash was known. A hardlink is repaired
  when the torrent's data is repaired, because repair rewrites the existing
  file in place. An independent copy is not.

## How it works

### 1. The durability barrier (always on)

Before a fastresume record is written, TorrentNG fsyncs the payload files it
has dirtied on that mount (`durability_mode` `checkpoint` and `strict`). Only
then does it write the record, itself via temp file, fsync, rename and
directory fsync. A crash at any point leaves either the old record or the new
one, and either is honest:

| Crash lands… | Disk holds | Restart sees |
|---|---|---|
| before the barrier finishes | old record, some new data | old record: new pieces are simply not claimed |
| between barrier and record | old record, all data | old record: nothing lost, pieces re-verified as needed |
| after the record | new record, all data | new record: every claim is backed by durable data |

The dirty-piece watermark in the record bounds recheck to pieces written since
the last completed barrier.

`durability_mode = "fast"` skips the payload fsync. Since this release such a
record is marked `synced = false`, and it is **discarded after a host crash**
(see [recovery](#3-host-crash-detection-and-recovery)); it is still trusted
after a clean shutdown or a process-only crash, because the page cache
survived. The final save when a torrent task exits (orderly shutdown, or an idle
torrent being demoted to dormant) **always runs a real barrier, even in `fast`
mode**, so "clean shutdown" means the data is on disk and not merely in the page
cache of a machine that could lose power a moment later. `fast` therefore trades
a full recheck after power loss for lower I/O during normal operation. It no
longer trades correctness.

### 2. The completion gate (`completion_gate`, default on)

The moment the last piece verifies, the torrent is **held**, not completed:

- state stays `downloading`;
- `amount_left` is reported as **1 byte** to the API, the trackers and every
  compatibility facade (qBittorrent, Transmission, Deluge, rTorrent), so
  automation that keys off "100%" or "seeding" does not import it;
- TorrentNG forces a real payload barrier (even in `fast` mode) and persists
  the resume record;
- only when both succeed does it become `seeding`, tell the tracker
  `completed`, and stamp `completed_at`.

If the barrier fails, the torrent stays held and retries with backoff (5 s
doubling to 60 s). A `completion_deferred` event is logged on the first
failure and `completion_released` when it clears. A filesystem that answers
`fsync` with "unsupported" (`ENOSYS`, `EINVAL`, `EOPNOTSUPP`; seen on some FUSE
mounts) is detected once, remembered, and released without a data sync rather
than held forever; that mount is then rated **weak**.

The 1-byte projection is a deliberate compatibility choice: it needs no new
state that every client API would have to learn about. The TorrentNG API also
publishes an explicit `finalizing: true` on the torrent (omitted when false;
runtime-only, never persisted), which the WebUI shows as **Finalizing**. Because
the label comes from that flag and not from the byte count, a torrent whose real
last piece is one byte long is not mislabeled.

### 3. Host crash detection and recovery (`host_crash_detection`, default on)

The daemon keeps a small marker, `run_marker.json` in the session directory:
the OS boot identity, a run id, a start time and a heartbeat (rewritten every
60 s), and a `graceful` flag set only after every torrent task has saved its
final state. On the next start:

| Marker | Boot identity | Verdict | Meaning |
|---|---|---|---|
| absent | – | `no_record` | first start, or upgraded; behaves exactly as before |
| `graceful` | any | `clean` | normal |
| not graceful | same boot | `process_crash` | machine stayed up; page cache survived; written data is intact |
| not graceful | different boot | `host_crash` | power loss, kernel panic, hard reset |
| not graceful, or unreadable | unknown | `unclean_unknown_cause` | cannot rule out power loss; treated as one |

A power loss and a `kill -9` need different handling, and this is the only
durable way to tell them apart. When boot identity is unavailable on a
platform the answer is deliberately pessimistic.

The marker only ever **adds** scrutiny. A lost or stale marker degrades to the
per-torrent checks that always existed.

After a `host_crash` (or unknown-cause unclean shutdown), each torrent's resume
record is judged by `host_crash_recovery`, first applying the mount's trust:

| Step (in order) | Result |
|---|---|
| 1. the record was written by **this** run | trust (it post-dates the crash) |
| 2. the record is `synced = false` (fast mode) | recheck |
| 3. escalation on and the mount is **weak** | policy becomes `full` |
| 3. escalation on and the mount is **unknown** | policy is raised to at least `recent` |
| 4. policy `full` | recheck |
| 4. policy `recent` and the torrent's payload was written within `recent_write_window_secs` before the crash | recheck |
| 4. otherwise (`watermark`, or `recent` outside the window) | trust; the dirty-piece watermark still applies |

"Recheck" discards the record so the existing durable, pausable recheck path
verifies the torrent. A torrent whose last write time is unknown (a record from
an older build) is *not* treated as recent, so the first crash after an upgrade
does not recheck the whole library for no evidence.

Each recheck is counted by reason (`torrentng_crash_recovery_rechecks_total`),
logged, and recorded as a `crash_recovery_recheck` event on the torrent. The
engine also records one `crash_recovery` event at startup.

### 4. Allocation audit (`structural_audit`, default `on_unclean`)

For the failure above, the strongest cheap signal is that **the filesystem
knows which parts of a file it never wrote.** A `fallocate`d region that was
never written is an *unwritten extent*; a truncated-up region is a *hole*. A
piece a resume record calls valid must not overlap either.

The audit asks the filesystem (`FS_IOC_FIEMAP` on Linux, falling back to
`SEEK_HOLE`/`SEEK_DATA`, which is also what macOS and FreeBSD use; allocated
ranges on Windows), reading **metadata only**, and downgrades any
overlapping `Valid` piece to `Unknown` so it is re-hashed. It runs on start
for `on_unclean` (any unclean previous run, or a record saved unclean),
`always`, or never (`off`).

It is a **prefilter, never a verdict**, on purpose:

- It only ever triggers a re-hash. Compressing and deduplicating filesystems
  (ZFS, btrfs) and sparse-aware copy tools legitimately store zero runs as
  holes, and BEP 47 pad files are all zeros. A false positive costs one
  re-hash of that piece; it never marks anything corrupt.
- It proves nothing when it finds nothing. A filesystem that wrote real zero
  blocks looks like data. This is why preallocation no longer uses
  `posix_fallocate` ([below](#5-preallocation)).
- Pad files are skipped. A file with more than about a million extents is
  reported as wholly suspect rather than scanned.

### 5. Preallocation

`preallocation_mode = "full"` (the default on non-CoW spinning disks) now calls
`fallocate(2)` directly. glibc's `posix_fallocate` **emulates** preallocation on
filesystems that lack it by writing a zero byte into every block, producing
"written" zeros that no audit can tell from data. A native `fallocate` leaves
unwritten extents the filesystem can report; where it is unsupported the call
fails with `EOPNOTSUPP` and TorrentNG falls back to a sparse length, which is
honest about holes.

### 6. Read-back verification (`completion_verify`, default `off`)

Optional, applied after the completion gate's barrier and before release:

| Mode | What is re-read from disk |
|---|---|
| `off` | nothing; the barrier alone gates release |
| `sample` | the first and last piece of every file (where a preallocation shortfall shows), plus a deterministic `completion_verify_sample_percent` % of all pieces, capped at 16,384 pieces |
| `full` | every piece |

Where the OS allows it, cached pages for the files are dropped first
(`posix_fadvise(DONTNEED)` on Linux) so the read comes from the device. On
platforms without a drop primitive the read-back may be served from the page
cache and is weaker; the WebUI shows this under platform support.

Verification runs through the normal recheck machinery, so it is visible as
`checking`, pausable and cancellable. While it runs the torrent keeps
reporting 1 byte remaining. A failed **sample** proves storage returned wrong
data, so it triggers a full recheck; the bad pieces are downloaded again. It is
counted (`torrentng_completion_verify_*`) and logged as
`completion_verify_failed`. Verification implies the barrier: enabling it while
`completion_gate = false` still runs the barrier.

Weak mounts get at least `sample` automatically when escalation is on.

### 7. Storage trust (`mount_probe`, `weak_mount_escalation`)

The barrier only helps if `fsync` on the mount reaches stable storage. TorrentNG
classifies each save path's mount from the filesystem type and, where the OS
exposes them, mount options (`/proc/self/mountinfo` on Linux; `statfs` on macOS
and FreeBSD, type only; the volume filesystem name and drive type on Windows,
where a remote drive is rated **unknown** whatever the server's filesystem is
and a RAM disk **weak**):

| Rating | Meaning | Examples |
|---|---|---|
| **Strong** | local block filesystem, no known reason to distrust `fsync` | ext4, xfs, btrfs, zfs, f2fs, apfs, ntfs |
| **Unknown** | behavior depends on config TorrentNG cannot inspect | nfs, cifs, 9p, virtiofs, fuse.\* (mergerfs, sshfs), overlay |
| **Weak** | known not to provide durable fsync | tmpfs, ramfs; any mount with `nobarrier`/`barrier=0`, `nostrictsync`, `cache=loose` |

This rates the **filesystem layer only.** It cannot see a disk's volatile cache,
a RAID controller, or a ZFS `sync=disabled` dataset. Use
`weak_mount_paths` / `strong_mount_paths` (longest prefix wins) to correct it.
With `weak_mount_escalation` on: weak mounts are fully rechecked after a host
crash and get a sampled read-back; unknown mounts get at least the
recent-writes check.

### 8. Integrity regressions

If any recheck (after a crash, on request, or from verification) finds pieces
invalid in a torrent that was **already reported complete**, TorrentNG:

- moves it back to downloading and re-downloads only the missing pieces,
  **writing into the existing files**, so hardlinks (the usual *arr import) are
  healed automatically;
- emits an `integrity_regression` event (level `error`) listing the trigger, the
  number of bad pieces, up to 32 piece indexes and up to 20 affected file
  paths, so an operator or automation can find copies that were made earlier and
  are *not* healed;
- counts it (`torrentng_integrity_regressions_total`) and raises a banner in
  the WebUI.

### 9. Pure BEP 52 (v2-only) torrents

The v2-only task never trusts a resume record: it re-hashes every file at
every start, so it cannot be fooled by a stale bitfield, and the host-crash
recovery policy does not apply to it. It otherwise matches the v1 task:

- the completion gate: nothing sees it complete before its files are fsynced,
  and it reports one byte remaining and `finalizing` while held; a filesystem
  that cannot `fsync` is detected and released;
- read-back verification and weak-mount escalation. A v2 file is verified whole
  against its Merkle root, so the unit of `sample` is the **file**: a
  deterministic subset covering at least `completion_verify_sample_percent` of
  the bytes (a single-file torrent is therefore always verified in full). A
  failed sample escalates to every file; failed files have their pieces cleared
  so they are downloaded again;
- `completion_verified`, `completion_verify_failed` and `integrity_regression`
  events, and the same counters.

Verification runs inline in the task, as a requested v2 recheck already does, so
pausing waits for it. Hybrid torrents use the v1 path.

## Options

All keys live under `[crash_safety]` in `config.toml`. Unknown keys are
**rejected** (config file and API) so a typo in a safety setting fails loudly
instead of silently leaving the default in force.

| Key | Default | Values | What it controls | Cost |
|---|---|---|---|---|
| `completion_gate` | `true` | bool | hold a finished download at "1 byte left" until data is synced | one disk sync per completed torrent |
| `host_crash_detection` | `true` | bool | keep the run marker so host crashes are detected | one small fsync'd write a minute |
| `host_crash_recovery` | `"recent"` | `"watermark"` `"recent"` `"full"` | how much to re-verify after a host crash | `full` reads the whole library |
| `recent_write_window_secs` | `86400` | 1 … 31 536 000 | look-back for `recent` | more torrents rechecked |
| `structural_audit` | `"on_unclean"` | `"off"` `"on_unclean"` `"always"` | metadata-only check for unwritten/hole extents | one allocation query per file at start |
| `mount_probe` | `true` | bool | classify each save location | negligible |
| `weak_mount_escalation` | `true` | bool | check harder on weak/unknown mounts | extra rechecks only on risky storage |
| `weak_mount_paths` | `[]` | absolute paths | force these prefixes to **weak** | – |
| `strong_mount_paths` | `[]` | absolute paths | force these prefixes to **strong** | – |
| `completion_verify` | `"off"` | `"off"` `"sample"` `"full"` | re-read finished downloads from disk before release | sample: a few %; full: doubles read I/O |
| `completion_verify_sample_percent` | `5` | 1 … 100 | sample size | proportional |
| `path_policies` | `[]` | up to 64 tables | per-location overrides of the gate, recovery, audit and read-back keys (below) | – |

Related keys that were already there: `[storage] durability_mode`
(`fast`/`checkpoint`/`strict`) and `[storage] preallocation_mode`.

```toml
[crash_safety]
# Everything shown is the default except completion_verify, which is opt-in.
completion_gate = true
host_crash_detection = true
host_crash_recovery = "recent"
recent_write_window_secs = 86400
structural_audit = "on_unclean"
mount_probe = true
weak_mount_escalation = true
weak_mount_paths = []
strong_mount_paths = []
completion_verify = "sample"
completion_verify_sample_percent = 5
```

### Per-location policies (`path_policies`)

One library rarely has one kind of storage: a local NVMe scratch disk, a mergerfs
pool and an NFS mount can sit behind the same daemon. `path_policies` lets each
location differ from the global settings. Each entry is a directory and any of
the five per-torrent keys; a key you leave out inherits the global value.

```toml
[crash_safety]
completion_verify = "off"            # global default

[[crash_safety.path_policies]]       # the NAS: read it back before Sonarr sees it
path = "/mnt/nas"
completion_verify = "sample"
completion_verify_sample_percent = 10
host_crash_recovery = "full"

[[crash_safety.path_policies]]       # a scratch disk whose contents are disposable
path = "/scratch"
completion_gate = false
structural_audit = "off"
```

Rules:

- A torrent's location is its **save path**. The policy whose `path` is the
  longest component-wise prefix of it applies (`/mnt/nas` matches `/mnt/nas/tv`,
  not `/mnt/nasty`).
- Policies **do not stack**. Only the winning policy is consulted; a key it
  leaves unset falls back to the *global* value, not to a shorter matching
  policy.
- Overridable per location: `completion_gate`, `host_crash_recovery`,
  `structural_audit`, `completion_verify`, `completion_verify_sample_percent`.
  The rest (`host_crash_detection`, `recent_write_window_secs`, `mount_probe`,
  `weak_mount_escalation`, and the mount-path lists) are about the daemon or the
  mount, not a torrent, and stay global.
- Weak/unknown-mount escalation still applies on top of a policy: a location
  set to `host_crash_recovery = "watermark"` is still rechecked in full when its
  mount is rated weak, and `completion_verify = "off"` is still raised to
  `sample` there. A policy can ask for more checking than escalation would; it
  cannot switch escalation off (turn `weak_mount_escalation` off for that).
- `path` must be absolute and unique within the list; at most 64 entries; a
  typo in a key is rejected like everywhere else.
- Editing a policy applies to torrents that start or finish afterward, exactly
  like the global settings. A torrent moved to a different save path is judged by
  its new location from then on.

**Check a location** (WebUI **Settings → Backend → Crash safety → Check a
location**, or `GET /api/v1/settings/crash-safety/path?path=/mnt/nas/tv`) shows
which policy matches a path, the resulting settings, the mount's rating and why,
and what recovery and read-back would actually run after escalation. The path
need not exist and is not added to the tracked mounts, so it is safe to use for
what-if questions.

### Runtime changes and precedence

`PUT /api/v1/settings/crash-safety` (and the WebUI **Apply** button) validates,
persists to the state database and applies immediately, for torrents that start
or finish afterward. That override survives restarts and **wins over the config
file** until you reset it (`DELETE /api/v1/settings/crash-safety`, or **Reset to
config file** in the UI). A stored override that no longer parses is ignored with
a warning; it never blocks startup. Switching `host_crash_detection` off removes
the marker; switching it on writes a new one.

### Presets (WebUI)

| Preset | gate | recovery | audit | read-back | escalation |
|---|---|---|---|---|---|
| Balanced (default) | on | recent, 24 h | on unclean | off | on |
| Maximum safety | on | full | always | full | on |
| Lean | on | watermark | off | off | off |

No preset switches the completion gate off. Presets never touch your mount-path
overrides.

### Choosing settings

- **Default seedbox, ext4/xfs, UPS or not:** leave everything at the default.
- **Huge library, reliable power:** `host_crash_recovery = "watermark"` keeps a
  post-crash recheck to the pieces written since the last barrier, plus anything
  the allocation audit flags.
- **Storage you do not trust** (consumer drives with write cache on, no UPS,
  hardware RAID without a battery, mergerfs/NFS): `host_crash_recovery = "full"`,
  `completion_verify = "sample"` or `"full"`, and list the mount in
  `weak_mount_paths`.
- **ZFS with `sync=disabled`:** the probe cannot see this. Put the dataset under
  `weak_mount_paths`.
- **Mixed storage:** keep the global settings for your best disk and add a
  `path_policies` entry for each location that needs more (or, for disposable
  scratch space, less).

## What you will see

**WebUI**: Settings → Backend → Crash safety shows how the last run ended, what
recovery did, live counters, per-mount ratings and every option with its cost. A
held torrent shows **Finalizing** in the table, detail panel and selection
panel. An integrity regression raises an alert banner. **Different settings for
specific folders** edits `path_policies` (unset options read "Use global
setting"), and **Check a location** answers what a folder would get.

**Events** (Settings → Backend → Operator Logs, `/api/v1/logs`,
`/api/v1/session-events`):

| Kind | Level | Meaning |
|---|---|---|
| `crash_recovery` | warn | startup: the previous run did not shut down cleanly; carries verdict, boot ids and the policy in force |
| `crash_recovery_recheck` | warn | a torrent's resume state was discarded; carries the reason and mount trust |
| `allocation_audit_downgraded` | warn | pieces overlapping unwritten/hole extents will be re-verified |
| `completion_deferred` | warn | download finished but its data is not yet durable; held |
| `completion_released` | info | the hold cleared (attempts and time waited) |
| `completion_verified` | info | read-back verification passed |
| `completion_verify_failed` | error | read-back found wrong data; bad pieces re-downloaded |
| `integrity_regression` | error | a completed torrent has pieces that no longer verify; affected files listed |
| `crash_safety_updated` | info | settings were changed at runtime |

**API**: [`GET/PUT/DELETE /api/v1/settings/crash-safety`](API.md#settings) and
`GET /api/v1/settings/crash-safety/path?path=` for a single location.
**Metrics**: `torrentng_crash_safety_previous_run_unclean`,
`torrentng_crash_recovery_rechecks_total{reason}`,
`torrentng_allocation_audit_pieces_downgraded_total`,
`torrentng_completions_gated_total`, `torrentng_completions_pending`,
`torrentng_completions_released_total`, `torrentng_completion_gate_retries_total`,
`torrentng_completion_verify_pieces_total`,
`torrentng_completion_verify_failures_total`,
`torrentng_integrity_regressions_total`, `torrentng_fsync_unsupported_total`,
`torrentng_storage_mounts{trust}`.

Suggested alerts: `torrentng_completions_pending > 0` for more than a few
minutes (storage cannot be synced); any increase in
`torrentng_integrity_regressions_total`; any increase in
`torrentng_completion_verify_failures_total`.

## Operating

**After a power loss.** Start the daemon and open Crash safety. It states the
verdict and what recovery did. Expect rechecks of recently written torrents;
they show as `checking` and are pausable. If `integrity_regressions` is above
zero, read the events: they list the affected files. Data that a hardlink shares
with the torrent is being repaired in place. Independent copies are not.

**A torrent stays at "Finalizing".** Storage cannot be synced (the barrier
keeps failing). Check `torrentng_completions_pending`, the `completion_deferred`
event and the daemon log (`component="torrent" operation="completion_gate"`).
Typical causes: the disk is full or failing, the mount went read-only, a network
mount dropped. It retries on its own and releases when storage recovers. A mount
that cannot `fsync` at all is detected and released automatically.

**Disabling.** Every protection can be turned off. Disabling
`host_crash_detection` restores pure per-torrent behavior; disabling
`completion_gate` restores immediate completion. Doing so reintroduces the
failure described at the top.

## Platform support

Correctness does not depend on this table: a probe that a platform cannot run
reports `unsupported` and steps aside, and the barrier, the gate, the recovery
decision and the run marker use portable code.

| Capability | Linux | macOS | FreeBSD | Windows |
|---|---|---|---|---|
| durability barrier, completion gate | yes | yes | yes | yes |
| run marker (crash vs clean) | yes | yes | yes | yes |
| boot identity (host vs process crash) | `boot_id` | `kern.boottime` | `kern.boottime` | uptime-derived boot time |
| allocation audit | FIEMAP (holes **and** unwritten extents), `SEEK_HOLE` fallback | `SEEK_HOLE` (holes) | `SEEK_HOLE` (holes) | allocated ranges (holes of sparse files only) |
| mount classification | `mountinfo` (type and options) | `statfs` type | `statfs` type | volume filesystem name and drive type |
| page-cache drop before read-back | `posix_fadvise` | no | `posix_fadvise` | no |
| preallocation | native `fallocate(2)` | sparse length | sparse length | sparse length |
| migration process inspection | `/proc` | `kill` + `ps` | `kill` + `ps` | `OpenProcess` |

Where a row says "no", the read-back may be served from the page cache, so it
is weaker there. Boot identity on macOS, FreeBSD and Windows is a boot *time*
compared with a 15-second tolerance, because a clock step moves it; a step
larger than that is read as a reboot, which only ever causes extra rechecking.
Windows has an analogue of an unwritten extent (NTFS valid data length) but no
documented API exposes it, so the Windows audit sees only holes in sparse
files.

**How each platform was verified**, so you know what you are trusting:

| Platform | Verification |
|---|---|
| Linux | Built and tested: the whole workspace, including a real `fallocate`d file on btrfs for the allocation audit. |
| Windows | Built for `x86_64-pc-windows-gnu`. The `rt-storage`, `rt-fastresume`, `rt-config` and `rt-piece-map` tests, the engine's crash-safety, run-marker, power-cut and v2 tests, and the migration process-inspection tests were **run under wine**. **Not run on real Windows.** Wine answers `ERROR_NOT_SUPPORTED` for the allocated-range query, so the success path of the Windows allocation audit was not exercised (the degrade-to-unsupported path was). Under wine, 31 storage-plan tests and 2 file-identity tests in `rt-storage` fail; the plan module had never built for Windows before, and the identity failures are wine reusing inode numbers after delete-and-recreate, so whether the plan failures are wine or real Windows gaps is undetermined. |
| macOS, FreeBSD | **Type-checked only**, never run: `rt-storage`, `rt-fastresume`, `rt-config`, `rt-piece-map`, and the migration process-inspection module in isolation. The daemon crates were not cross-checked (bundled SQLite needs a C toolchain for those targets). |

Windows support also required replacing two uses of the unstable
`MetadataExt::volume_serial_number`/`file_index` in `rt-storage` with a
hand-declared `GetFileInformationByHandle` (`win32.rs`), since the crate did not
compile for Windows otherwise.

## Migration

Importing another client's resume state and trusting it reproduces the failure
if that client crashed. `torrentngd migrate` therefore checks the source's
shutdown state before applying a trusting policy (see
[MIGRATION.md](MIGRATION.md#crash-detection)):

- **rTorrent:** a leftover `rtorrent.lock` whose process is gone means it
  crashed; the lock's location is fixed, so no lock means a clean exit.
- **qBittorrent, Deluge:** best-effort search for `lockfile` / `deluged.pid` near
  the state directory. Those names and formats are from the clients' documented
  behavior, **not verified against the real clients**, so a missing file is
  reported as *unknown*, never *clean*.
- **Any other client:** pass `--source-lock <FILE>` (for example
  `transmission-daemon --pid-file`).

A crashed source lowers the import policy to `verify`; a running one is refused.
With no signal, after any crash of that client use `--policy verify`.

## Design notes and rejected alternatives

- **Write into a temporary name or an incomplete directory and rename into
  place on completion** (as qBittorrent's `.!qB` suffix or Transmission's
  `.part`/incomplete-dir do). Rejected: the guarantee it gives, "nothing outside
  sees an unfinished file", is what the completion gate provides in the
  application, on every OS, without renaming files, cross-filesystem moves,
  hardlink and migration breakage, or client-specific path conventions. It also
  does not help after a crash: a renamed file with lost data is still wrong.
- **Per-piece fsync.** Rejected: fatal on spinning disks at scale. One barrier
  per mount per save, and one at completion, gives the same ordering guarantee.
- **Treat holes as corruption.** Rejected: ZFS/btrfs compression and sparse
  copies make legitimate holes; the audit only triggers a re-hash.
- **A new public `finalizing` torrent state.** Rejected: every compatibility
  API would need to learn it. One byte remaining is understood by all of them.
- **Trust `fast` mode after a host crash.** Rejected: that is the original bug.

## Testing

`crates/rt-engine` includes a **power-cut model**: a two-image disk (`live`
page cache and `durable`), a randomized walk of writes and saves, and a "cut the
power" after every step that rebuilds the payload from the durable image, keeps
the fastresume file as it was, and checks that no piece the loader trusts hashes
wrong. It passes for `checkpoint` and for `fast` mode with detection on, and it
**fails** for `fast` mode with detection off, so the harness demonstrably can
catch the bug. Further tests cover the gate under failing persistence, recovery
windows and mount escalation, the allocation audit against a real `fallocate`d
file, sampled and full read-back including corruption and escalation, integrity
regressions, the run marker across real engine restarts, settings
persistence, and per-location policies (longest-prefix resolution, inheritance,
non-stacking, escalation on top of a policy, and that the gate, recovery and
read-back each honor the torrent's own location, for v1 and v2 tasks). A real
`torrentngd` process is also started, killed with `SIGKILL` and restarted to
prove the daemon reports the crash (`crates/torrentngd/tests/crash_recovery_process.rs`).

Run: `cargo test -p rt-engine --lib -- crash power_cut completion allocation_audit`.

The model cannot simulate hardware that lies about flushes; that is what the
mount classification, read-back and `weak_mount_paths` are for.

## Known limits

- Only Linux was tested natively. Windows ran under wine, macOS and FreeBSD were
  type-checked (see the platform table); a real-hardware run on those is still
  owed, and the Windows allocation-audit success path has not run anywhere.
- macOS and Windows have no page-cache drop, so read-back verification there may
  be served from cache.
- The Windows audit sees holes in sparse files only, not NTFS never-written
  tails.
- A v2 `sample` verifies whole files, so a single-file v2 torrent is always
  verified in full.
- A torrent held by the gate does not send periodic `left=0` announces; the
  tracker sees it as a leecher until release, then `completed`.
- Migration auto-detects a crash only for rTorrent with confidence; the
  qBittorrent and Deluge lock locations are best effort and unverified, and
  other clients need `--source-lock`.
- `weak_mount_paths`/`strong_mount_paths` and `path_policies` must be absolute
  and are matched by path prefix on the torrent's save path, not by resolved
  mount: a symlink or bind mount that leads to the same storage under a
  different name does not inherit a policy written for the other name.
