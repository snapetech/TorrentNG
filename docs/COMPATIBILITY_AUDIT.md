# Client Compatibility Audit

This is the working ledger for migration/import and facade compatibility. It is
source-driven: every supported client family has an upstream API or state source,
the local implementation entry point, and explicit gaps.

For the broader feature, API, field, and test backlog matrices, see
`docs/CLIENT_COMPATIBILITY_MATRICES.md`.

Primary references checked on 2026-05-17:

- qBittorrent WebUI API 5.0:
  https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29
- Transmission RPC specification:
  https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md
- Deluge Web JSON-RPC API:
  https://deluge.readthedocs.io/en/deluge-2.0.5/reference/webapi.html
- Deluge core RPC API:
  https://deluge.readthedocs.io/en/deluge-2.0.4/reference/api.html

## Import Coverage

| Source client | Local entry point | State inputs | Preserved fields | Confidence |
|---|---|---|---|---|
| qBittorrent / libtorrent | `dry_run_qbittorrent` | `.torrent`, `.fastresume`, aggregate resume dictionaries | save path, category, tags, counters, added/completed timestamps, paused/completed state, piece states, partial blocks, file priority/wanted/completed bytes, tracker timestamps/results/counts | Trusted when piece data matches torrent piece count |
| rTorrent | `dry_run_rtorrent` | session `.torrent` files and complete/missing file hints | info hash, name, save path, size, completion inference | Hints unless verified from files |
| Transmission | `dry_run_transmission` | `.torrent`, resume sidecars, JSON/bencode resume dictionaries | save path/download dir, labels, counters, lifecycle timestamps, paused/completed state, file priority/wanted/completed bytes, bitfield/have/valid progress, tracker stats | Trusted when bitfield or valid pieces are present |
| Deluge | `dry_run_deluge_state` | `state` directory, `.torrent`, `.fastresume`, JSON/bencode state | download location, label/category, counters, lifecycle timestamps, paused/completed state, file priority/wanted/completed bytes, libtorrent resume data, tracker stats | Trusted when resume data contains piece state |
| uTorrent / BitTorrent Classic | `dry_run_utorrent_config` | `resume.dat`, sidecar resume dictionaries | raw/hex/base32 info hash keys, path, label, counters, timestamps, state flags, piece/bitfield progress | Trusted when keyed resume matches torrent hash |
| BiglyBT / Vuze | `dry_run_biglybt_config` | `downloads.config`, `torrents.config`, nested resume dictionaries | hex/nested hash keys, save path, category, tags, counters, timestamps, file progress, tracker activity | Hints to trusted depending on nested resume content |
| Tixati | `dry_run_tixati_config` | config directory scan, `.torrent`, scannable sidecars | generic path/category/tags/counters/timestamps/progress hints | Hints; proprietary fields must stay verification-first |
| Generic torrent directory | `dry_run_generic_torrent_directory` | recursive `.torrent` scan plus adjacent sidecars | torrent metadata, path remaps, optional resume hints | MetadataOnly to Trusted depending on sidecars |

Shared import features:

- Bencode and JSON sidecars.
- Aggregate containers named `resume.dat`, `downloads.config`, `torrents.config`.
- Hash matching by raw SHA-1 bytes, lowercase/uppercase hex, base32, and torrent stem.
- Aggregate resume detection is source-independent for known aggregate filenames
  such as `resume.dat`, `downloads.config`, and `torrents.config`.
- Path remapping through `ImportOptions::path_remaps`.
- Size limits and symlink-safe directory walking.
- Conversion into native fastresume state through `MigrationPlan::to_fastresume_import`,
  `apply_fastresume`, and `apply_native_import`.

Known import limits:

- Tixati and some BiglyBT/Vuze private fields are intentionally verification-first
  because public, stable state schemas are limited.
- Piece state is downgraded when resume data conflicts with torrent metadata.
- Client-specific scheduler/RSS/search histories are not native torrent progress
  and are not imported yet.

## qBittorrent API Facade

Local surface: `crates/rt-api-qbit`.

| API group | Upstream surface | Local status |
|---|---|---|
| Authentication | `auth/login`, `auth/logout` | Implemented |
| Application | version, API version, build info, shutdown, preferences, set preferences, default save path, network interface probes, test email | Implemented as native/no-op compatibility where no native equivalent exists |
| qBittorrent 5 cookie APIs | `app/getCookies`, `app/setCookies` | Implemented as empty/no-op compatibility |
| Logs | main log, peer log | Implemented as empty-compatible reads |
| Sync | `sync/maindata`, `sync/torrentPeers` | Implemented |
| Transfer | global info, speed limits, speed-limit mode, ban peers | Implemented |
| Torrent reads | list, properties, trackers, web seeds, files, piece states, piece hashes, export | Implemented |
| Torrent lifecycle | add, pause/resume legacy aliases, start/stop v5 aliases, delete, recheck, reannounce | Implemented |
| Torrent mutation | tracker add/edit/remove, peers, priority order, file priority, limits, share limits, force start, super seeding, auto management, sequential, first/last, location/save path, rename, category, tags | Implemented |
| RSS | folders, feeds, items, rules, matching articles | Implemented as no-op/read-compatible |
| Search | status, categories, plugins, plugin mutation, start/stop/results/delete | Implemented as no-op/read-compatible |

## Transmission RPC Facade

Local surface: `crates/rt-api-transmission`.

| API group | Upstream surface | Local status |
|---|---|---|
| Protocol shape | Transmission 4.1 JSON-RPC 2.0 uses snake_case; older RPC uses kebab/camel strings | Implemented: snake_case methods and args normalize to native handlers; snake_case callers receive snake_case response keys |
| CSRF session ID | `X-Transmission-Session-Id` retry flow | Implemented |
| Session reads | `session_get`, `session_stats`, `session_access_control` | Implemented |
| Session writes | `session_set`, `session_close`, queue-stalled enable/disable | Implemented for native limits and compatibility no-ops |
| Groups | `group_get`, `group_set` | Accepted as compatibility no-ops |
| Torrent actions | start, start now, stop, verify, reannounce, remove | Implemented |
| Torrent add | filename magnet, base64 metainfo, paused, download dir, labels | Implemented |
| Torrent reads | `torrent_get` common fields: identity, status, size/progress, counters, labels, paths, files, file stats, trackers, tracker stats, peers, queue, dates, private flag, magnet link | Implemented |
| Torrent writes | `torrent_set`, tracker list, file priorities, wanted/unwanted, location, rename path | Implemented where native engine supports it; accepted as no-op otherwise |
| Utility | `port_test`, `blocklist_update`, `free_space` | Implemented as compatibility responses |
| Remaining field depth | detailed availability depth, group internals, live webseed activity, detailed session script/blocklist/preferred transport settings | Gap or compatibility placeholder |

## Deluge JSON-RPC Facade

Local surface: `crates/rt-api-deluge`.

| API group | Upstream surface | Local status |
|---|---|---|
| JSON endpoint | `/json`, `/deluge/json` | Implemented |
| Auth/daemon | login, check session, daemon login/info/method list/shutdown | Implemented |
| Web host management | connected, hosts, host status, connect/disconnect/start/stop daemon | Implemented as native/no-op compatibility |
| Web UI | update UI, events, torrent files, plugin list/info/upload/update/save config | Implemented |
| Core session reads | stats, session status/state, rates, connections, filter tree, cache status, config, config values, config value, free space, listen port, external IP, path size, libtorrent version | Implemented |
| Core torrent reads | torrents status, torrent status, torrent file status | Implemented |
| Core torrent lifecycle | add magnet, add torrent file, pause, resume, force recheck, remove | Implemented |
| Core torrent mutation | queue movement, set options, file priorities, trackers, prioritize first/last, connect peer, rename files/folder, move storage | Implemented where native engine supports it; accepted as no-op otherwise |
| Plugin APIs | label, notifications | Implemented for common label and notification calls |
| Remaining Deluge plugins | extractor, scheduler, execute, blocklist, autoadd plugin-specific APIs | Gap unless required by a target migration/client |

## Cross-Client API Backlog

| Priority | Work item | Why |
|---|---|---|
| Done | Add tests that enumerate advertised facade methods and assert every advertised method returns a compatibility-shaped response | Prevents method-list drift |
| Medium | Add qBittorrent preference-key breadth for modern WebUI clients | Avoids settings panes seeing absent keys |
| Medium | Import scheduler/RSS/search metadata as auxiliary migration artifacts | Useful for full client migration, not required for torrent progress |
| Low | Proprietary client deep parsers for Tixati/BiglyBT plugin-only fields | Needs fixture corpus; progress import already uses generic verified paths |
