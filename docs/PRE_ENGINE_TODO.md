# Pre-Engine Completion TODO

Historical archive. This list covered work before the native BitTorrent engine
rewrite. The native engine now has its own implementation and certification
surface; see [ENGINE_REWRITE.md](ENGINE_REWRITE.md),
[ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md), and
[NATIVE_DEPLOYMENT.md](NATIVE_DEPLOYMENT.md).

## Done in sidecar scope

- Native REST API for torrents, files, trackers, categories, tags, bulk operations, settings, storage, tracker health, ratio groups, workflows, RSS rules, cross-seed helper, and saved views.
- qBittorrent compatibility shim for automation/client flows, including auth, app info, torrent CRUD/control, tracker ops, file priorities, categories, tags, RSS rules, sync/maindata, and transfer info.
- WebUI for torrent list/detail, add dialog, categories/tags, bulk dry-run operations, storage, tracker health, ratio groups, workflows, RSS rules, and sidecar-backed saved views.
- Workflow actions for webhook, script execution with explicit config gate, category changes, and location changes.
- Deployment scaffolding for Docker, Phase 1 ruTorrent bundle, systemd, nginx, healthcheck, and migration docs.
- Synthetic benchmark harness for qBit list and sync delta targets.
- Docker Compose certification stack for Sonarr, Radarr, Prowlarr, autobrr, cross-seed, and rtorrentNG with a repeatable certification runner.
- Local live certification report passing for sidecar health, qBit auth/read APIs, Sonarr, Radarr, Prowlarr, autobrr, and cross-seed container readiness.
- Repeatable client-configuration runner that onboards autobrr and saves tested qBittorrent-compatible rtorrentNG clients in Sonarr, Radarr, Prowlarr, and autobrr.
- Local live transfer certification runner that creates a fixture torrent, seeds it from stock Transmission through a disposable local tracker, downloads it through rtorrentNG, and smoke-tests public Linux torrent URL add.
- App-driven Prowlarr certification runner that stands up a disposable Torznab fixture indexer, searches it through Prowlarr, grabs the release through the saved qBittorrent-compatible rtorrentNG client, and verifies the completed transfer.
- Sonarr/Radarr app certification runner that creates disposable Torznab fixture indexers, saves app indexers, verifies app-level release search/cache, and can submit app-level release grabs through the saved qBittorrent-compatible rtorrentNG client.
- autobrr certification runner that verifies login/onboard, qBittorrent downloader reachability, filter creation, qBit action creation, and readback against the saved rtorrentNG client.
- Isolated release-grab certification runner that starts a separate normal-sync certification stack on non-conflicting ports, then proves live readiness, client configuration, Prowlarr release grab/transfer, Sonarr release grab/transfer, Radarr release grab/transfer, and autobrr downloader/filter/action setup without interrupting the primary 15k soak stack.
- Benchmark report runner covering 1k, 10k, 15k, and 50k synthetic libraries; latest local report passed the list and sync delta targets.
- Soak certification runner that samples health, qBit list/sync behavior, torrent count, and container RSS for a configurable duration; use `SOAK_DURATION_SECONDS=86400` for the release 24-hour gate.
- Live cache scale seeder for certification stacks; use `RTNG_SEED_TORRENTS=15000 scripts/seed_live_cache.sh` before soak to exercise 15k qBit list/sync surfaces without requiring 15k real torrent sessions.
- DHT certification runners covering rTorrent DHT/listen port wiring, PEX, UDP tracker support, optional VPN public endpoint evidence, LAN NAT-PMP mapping evidence, and Proton NAT-PMP mapping through the existing slskR WireGuard namespace harness.
- Phase 1 ruTorrent certification runner covering rTorrent/libtorrent 0.16.11 evidence, ruTorrent 5.3.1 assets, PHP-FPM/nginx, container health, SCGI socket readiness, and live incoming listener state.
- Live mobile compatibility script for NZB360/Transdrone-style qBittorrent read flows across `/api/qb/v2` and `/api/v2`.
- Certification status summarizer, normal-mode restore helper, and security scan wrapper for npm, cargo dependency resolution, and container image scanning.
- Soak finalizer that validates completed 24-hour reports, enforces sample/torrent/RSS/HTTP criteria, and can restore normal certification sync mode.
- Soak status and post-soak release-gate runners that summarize active long-soak health, then finalize, restore normal sync, rerun the short suite, and refresh the release report after the 24-hour gate completes.
- Pre-engine release report and suite runners that summarize the full evidence corpus and refresh all short/non-24h automated gates in one pass.
- Script workflow security review checklist and configuration policy.

## External release gates

- Drive an actual autobrr announce ingest from a live IRC/indexer source if tracker credentials are available; local autobrr downloader/filter/action setup is covered, and Prowlarr/Sonarr/Radarr app-level fixture grabs now complete through rtorrentNG.
- Run live compatibility certification against real mobile NZB360 and Transdrone clients and attach the generated report; the script-level qBittorrent read-flow certification is available as `scripts/mobile_compat_certification.sh`, but app UI certification still needs physical/mobile clients or emulators with the apps installed.
- Run benchmark report on target release hardware and publish results for realistic imported libraries in addition to the synthetic matrix.
- Run long soak test for memory and sync stability at 15k torrents for 24 hours on target release hardware using `scripts/soak_certification.sh`.
- Complete independent security review of script workflow policy before recommending it for production.
- Re-run `scripts/phase1_certification.sh` immediately before release and rebuild the Phase 1 image if upstream ruTorrent/PHP base behavior changes.

## Moved to native engine rewrite

- Historical section: these items moved into the native rewrite work tracked in
  [ENGINE_REWRITE_BURNDOWN.md](ENGINE_REWRITE_BURNDOWN.md).
