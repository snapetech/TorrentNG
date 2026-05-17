# rtorrentNG Docs

Start here when choosing, testing, or operating an rtorrentNG engine mode.

## Engine Modes

- [ENGINE_REWRITE.md](ENGINE_REWRITE.md) - practical guide to the native rewrite,
  the rTorrent-backed mode, how to swap between them, and what behavior differs.
- [ENGINE.md](ENGINE.md) - deeper native Rust engine design and crate layout.
- [ARCHITECTURE.md](ARCHITECTURE.md) - runtime component diagrams for native
  mode and Track 1 sidecar mode.
- [ROADMAP.md](ROADMAP.md) - historical track plan and current native rewrite
  acceptance criteria.

## Deploy And Operate

- [NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md) - production `rusttorrentd`
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
- [INTEGRATION_COMPAT.md](INTEGRATION_COMPAT.md) - sidecar and native
  compatibility test coverage.
- [INTEROP_MATRIX.md](INTEROP_MATRIX.md) - Docker client matrix across
  rusttorrentd, qBittorrent, Transmission, Deluge, rTorrent, local fixtures, and
  official public Linux torrents.
- [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md) - implementation
  checklist for the native rewrite.

## Security And Review

- [SECURITY_REVIEW.md](SECURITY_REVIEW.md) - script workflow and sidecar policy
  review notes.
- [THREAT_MODEL.md](THREAT_MODEL.md) - threat model for exposed surfaces.
- [AUDIT.md](AUDIT.md) - Track 1 rTorrent/ruTorrent audit and mitigations.
- [PRE_ENGINE_TODO.md](PRE_ENGINE_TODO.md) - historical archive of pre-native
  completion work.
