# TorrentNG Docs

Start here when choosing, testing, or operating TorrentNG.

TorrentNG is a unified BitTorrent WebUI and automation API. It can sit on top
of a compatible client that is already managing a library, or it can use the
next-generation first-party TorrentNG client, `torrentngd`. The documentation
tracks both the common UI/API contract and the backend-specific capabilities;
compatible does not mean every client exposes identical features.

## Product arrangements

- [ENGINE_REWRITE.md](ENGINE_REWRITE.md) - practical guide to compatible-client
  integrations, the TorrentNG client, and migration/comparison behavior.
- [ENGINE.md](ENGINE.md) - deeper TorrentNG client design and crate layout.
- [ARCHITECTURE.md](ARCHITECTURE.md) - runtime component diagrams for the
  WebUI, compatible clients, and the TorrentNG client.
- [ROADMAP.md](ROADMAP.md) - historical track plan and current TorrentNG client rewrite
  acceptance criteria.
- [PROJECT_GAP_AUDIT.md](PROJECT_GAP_AUDIT.md) - current whole-project gap
  audit across roadmaps, storage, memory, WebUI, compatibility, interop,
  security, and release evidence.
- [BACKEND_AUDIT_BURN_DOWN.md](BACKEND_AUDIT_BURN_DOWN.md) - canonical,
  evidence-backed remediation ledger for the TorrentNG client/backend audit; this is the
  release-facing source of truth for open findings and unsupported claims.
- [BACKEND_BURNDOWN_RELEASE_20260902.md](BACKEND_BURNDOWN_RELEASE_20260902.md) -
  exact release-artifact smoke evidence and explicit blockers from the focused
  backend burn-down run.
- [WEBUI_AUDIT.md](WEBUI_AUDIT.md) - torrent-table alignment, status semantics,
  field coverage, and the remaining cross-client projection gaps.
- [RELEASE_EVIDENCE.md](RELEASE_EVIDENCE.md) - operator runbook for clearing
  warning rows, enforcing strict readiness, and packaging release evidence.

## Deploy And Operate

- [TorrentNG client deployment guide](NATIVE_DEPLOYMENT.md) - production
  `torrentngd` deployment with Compose, systemd, Kubernetes, metrics, and certification.
- [DEPLOYMENT.md](DEPLOYMENT.md) - compatible-client WebUI/API deployment.
- [CONFIGURATION.md](CONFIGURATION.md) - TorrentNG client and compatible-client
  WebUI/API service configuration.
- [TRACKER-IDENTITY.md](TRACKER-IDENTITY.md) - rTorrent/libtorrent tracker
  identity policy for the compatible-client integration and TorrentNG client.
- [BACKUP_RESTORE.md](BACKUP_RESTORE.md) - TorrentNG-client state backup, restore, and
  migration rollback.
- [MIGRATION.md](MIGRATION.md) - `torrentngd migrate` (import) and
  `torrentngd export` (reverse / leave) for rTorrent, qBittorrent, Deluge,
  Transmission, uTorrent, BiglyBT, Tixati, and generic state, with fidelity
  rules and rollback.

## APIs And Compatibility

- [API.md](API.md) - TorrentNG REST, qBittorrent-compatible, Transmission, Deluge,
  health, metrics, and auth surfaces.
- [RTORRENT_LIBRARY_API.md](RTORRENT_LIBRARY_API.md) - the explicitly
  library-only rTorrent XML-RPC boundary, credential contract, and why it is
  not mounted as an HTTP server.
- [CLIENT_COMPATIBILITY_MATRICES.md](CLIENT_COMPATIBILITY_MATRICES.md) -
  capability-aware compatibility target, current status, and certification backlog.
- [INTEGRATION_COMPAT.md](INTEGRATION_COMPAT.md) - compatible-client and
  TorrentNG client compatibility test coverage.
- [INTEROP_MATRIX.md](INTEROP_MATRIX.md) - Docker client matrix across
  torrentngd, qBittorrent, Transmission, Deluge, rTorrent, local fixtures, and
  official public Linux torrents.
- [STORAGE_PHASE_B_TEST_MATRIX.md](STORAGE_PHASE_B_TEST_MATRIX.md) - focused
  matrix for storage topology, auto preallocation, peer-read locality, and
  per-device elevator work.
- [STORAGE_MEMORY_GAP_REGISTER.md](STORAGE_MEMORY_GAP_REGISTER.md) - current
  storage and memory gaps that still need implementation or hardware evidence.
- [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md) - implementation
  checklist for the TorrentNG client rewrite.

## Security And Review

- [SECURITY_REVIEW.md](SECURITY_REVIEW.md) - script workflow and WebUI/API
  service policy review notes.
- [THREAT_MODEL.md](THREAT_MODEL.md) - threat model for exposed surfaces.
- [AUDIT.md](AUDIT.md) - Historical Track 1 rTorrent/ruTorrent integration audit and mitigations.
- [PRE_ENGINE_TODO.md](PRE_ENGINE_TODO.md) - historical archive of pre-TorrentNG-client
  completion work.
