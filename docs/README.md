# TorrentNG Docs

Start here when choosing, testing, or operating an TorrentNG engine mode.

TorrentNG's product goal is universal torrent-client compatibility: import
from existing clients, expose the APIs automation tools already speak,
interoperate with independent BitTorrent clients, and provide a native Rust
engine that can replace older cores without forcing a workflow reset. The docs
track both sides of that goal: the target compatibility surface and the current
certified status.

## Engine Modes

- [ENGINE_REWRITE.md](ENGINE_REWRITE.md) - practical guide to the native rewrite,
  the rTorrent-backed mode, how to swap between them, and what behavior differs.
- [ENGINE.md](ENGINE.md) - deeper native Rust engine design and crate layout.
- [ARCHITECTURE.md](ARCHITECTURE.md) - runtime component diagrams for native
  mode and Track 1 sidecar mode.
- [ROADMAP.md](ROADMAP.md) - historical track plan and current native rewrite
  acceptance criteria.

## Deploy And Operate

- [NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md) - production `torrentngd`
  deployment with Compose, systemd, Kubernetes, metrics, and certification.
- [DEPLOYMENT.md](DEPLOYMENT.md) - Track 1 rTorrent plus sidecar deployment.
- [CONFIGURATION.md](CONFIGURATION.md) - native and sidecar config references.
- [BACKUP_RESTORE.md](BACKUP_RESTORE.md) - native state backup, restore, and
  migration rollback.
- [MIGRATION.md](MIGRATION.md) - importing state from rTorrent, qBittorrent, and
  Transmission.

## APIs And Compatibility

- [API.md](API.md) - native REST, qBittorrent-compatible, Transmission, Deluge,
  health, metrics, and auth surfaces.
- [CLIENT_COMPATIBILITY_MATRICES.md](CLIENT_COMPATIBILITY_MATRICES.md) -
  universal compatibility target, current status, and certification backlog.
- [INTEGRATION_COMPAT.md](INTEGRATION_COMPAT.md) - sidecar and native
  compatibility test coverage.
- [INTEROP_MATRIX.md](INTEROP_MATRIX.md) - Docker client matrix across
  torrentngd, qBittorrent, Transmission, Deluge, rTorrent, local fixtures, and
  official public Linux torrents.
- [STORAGE_PHASE_B_TEST_MATRIX.md](STORAGE_PHASE_B_TEST_MATRIX.md) - focused
  matrix for storage topology, auto preallocation, peer-read locality, and
  per-device elevator work.
- [STORAGE_MEMORY_GAP_REGISTER.md](STORAGE_MEMORY_GAP_REGISTER.md) - current
  storage and memory gaps that still need implementation or hardware evidence.
- [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md) - implementation
  checklist for the native rewrite.

## Security And Review

- [SECURITY_REVIEW.md](SECURITY_REVIEW.md) - script workflow and sidecar policy
  review notes.
- [THREAT_MODEL.md](THREAT_MODEL.md) - threat model for exposed surfaces.
- [AUDIT.md](AUDIT.md) - Track 1 rTorrent/ruTorrent audit and mitigations.
- [PRE_ENGINE_TODO.md](PRE_ENGINE_TODO.md) - historical archive of pre-native
  completion work.
