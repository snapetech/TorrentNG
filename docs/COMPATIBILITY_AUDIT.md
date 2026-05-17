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
- Synthetic matrix coverage for common JSON fields and client-specific bencoded
  aliases across qBittorrent, Transmission, Deluge, uTorrent/BitTorrent Classic,
  BiglyBT/Vuze, Tixati, and generic directories.
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
- Client-specific scheduler/RSS/search/plugin histories are not native torrent
  progress; migration dry runs preserve them as auxiliary artifacts so users can
  carry the files forward without mixing them into piece/progress state.

## qBittorrent API Facade

Local surface: `crates/rt-api-qbit`.

| API group | Upstream surface | Local status |
|---|---|---|
| Authentication | `auth/login`, `auth/logout` | Implemented |
| Application | version, API version, build info, shutdown, preferences, set preferences, default save path, network interface probes, test email | Implemented as native/no-op compatibility where no native equivalent exists; `setPreferences` persists arbitrary qBit preference keys in facade state |
| qBittorrent 5 cookie/API key APIs | `app/getCookies`, `app/setCookies`, `app/rotateAPIKey`, `app/deleteAPIKey` | Implemented as empty/no-op compatibility |
| Logs | main log, peer log | Implemented as empty-compatible reads |
| Sync | `sync/maindata`, `sync/torrentPeers` | Implemented; maindata carries broad torrent/server-state keys and torrentPeers carries qBit peer shape with stable RID deltas |
| Transfer | global info, speed limits, speed-limit mode, ban peers | Implemented |
| Torrent reads | list with modern qBit path/counter/limit/mode/magnet/infohash fields, properties, trackers, web seeds, files, piece states, piece hashes, export | Implemented; properties include registry/engine-backed counters, lifecycle times, piece counts, and limits where available |
| Torrent lifecycle | add, pause/resume legacy aliases, start/stop v5 aliases, delete, recheck, reannounce | Implemented |
| Torrent mutation | tracker add/edit/remove, peers, priority order, file priority, limits, share limits, force start, super seeding, auto management, sequential, first/last, location/save path, rename, category with configured save paths, tags with global tag cleanup | Implemented |
| RSS | folders, feeds, items, rules, matching articles | Implemented as no-op/read-compatible |
| Search | status, categories, plugins, plugin mutation, start/stop/results/delete | Implemented as no-op/read-compatible |

## Transmission RPC Facade

Local surface: `crates/rt-api-transmission`.

| API group | Upstream surface | Local status |
|---|---|---|
| Protocol shape | Transmission 4.1 JSON-RPC 2.0 uses snake_case; older RPC uses kebab/camel strings | Implemented: JSON-RPC 2.0 single and batch envelopes with `params`, direct `result`, and error objects; snake_case methods/args normalize to native handlers; old envelope remains supported |
| CSRF session ID | `X-Transmission-Session-Id` retry flow | Implemented |
| Session reads | `session_get`, `session_stats`, `session_access_control` | Implemented; `session_get` supports field projection |
| Session writes | `session_set`, `session_close`, queue-stalled enable/disable | Implemented for native limits plus broad compatibility-state roundtrip for paths, queues, scheduler, peer/network, blocklist, scripts, and seeding settings |
| Groups | `group_get`, `group_set` | Accepted as compatibility no-ops |
| Torrent actions | start, start now, stop, verify, reannounce, remove | Implemented |
| Torrent add | filename magnet, base64 metainfo, paused, download dir, labels | Implemented |
| Torrent reads | `torrent_get` common fields: identity, status, size/progress, counters, labels, paths, files, file stats, trackers, tracker stats, peers, queue, dates, private flag, magnet link; object and table formats; recently-active removed list | Implemented |
| Torrent writes | `torrent_set`, tracker list, file priorities, wanted/unwanted, location, rename path | Implemented where native engine supports it; labels, location, speed limits, peer limits, seed limits, and sequential mode roundtrip in facade state when no engine is attached |
| Utility | `port_test`, `blocklist_update`, `free_space` | Implemented as compatibility responses |
| Remaining field depth | detailed availability depth, group internals, live webseed activity, and native-backed effects for script/blocklist/preferred transport settings | Gap or compatibility placeholder |

## Deluge JSON-RPC Facade

Local surface: `crates/rt-api-deluge`.

| API group | Upstream surface | Local status |
|---|---|---|
| JSON endpoint | `/json`, `/deluge/json` | Implemented |
| Auth/daemon | login, check session, daemon login/info/method list/shutdown | Implemented |
| Web host management | add/edit/remove host, connected, hosts, host status, connect/disconnect/start/stop daemon | Implemented as native/no-op compatibility |
| Web UI | add torrents, URL download placeholder, update UI, events, torrent files, plugin list/info/upload/update/save config | Implemented; `web.add_torrents` accepts common magnet, embedded metainfo, temp-file path, and URL-placeholder shapes; update UI honors requested torrent fields and emits filter/stat shape; URL download does not perform server-side network fetch |
| Core session reads | stats, session status/state, rates, connections, filter tree, cache status, config, config values, config value, free space, listen port, external IP, path size, libtorrent version | Implemented |
| Core torrent reads | torrents status, torrent status, torrent file status | Implemented with requested-field projection, label/state/hash filter dictionaries, and torrent option projection |
| Core torrent lifecycle | add magnet, add torrent file, pause, resume, force recheck, remove | Implemented |
| Core torrent mutation | queue movement, set options, file priorities, trackers, prioritize first/last, connect peer, rename files/folder, move storage | Implemented where native engine supports it; torrent options roundtrip in facade state when no engine is attached |
| Plugin APIs | label, notifications | Implemented for common label and notification calls |
| Remaining Deluge plugins | extractor, scheduler, execute, blocklist, autoadd plugin-specific APIs | API gap unless required by a target migration/client; migration dry runs preserve matching plugin/config files as auxiliary artifacts |

## rTorrent XMLRPC Facade

Local surface: `crates/rt-api-rtorrent`.

| API group | Upstream surface | Local status |
|---|---|---|
| System/session/network | `method.list`, `system.*`, `session.*`, `network.*`, throttle reads | Implemented as compatibility dispatcher with stable version/session/network values and zero-rate throttle placeholders |
| Download reads | `d.hash`, `d.name`, `d.base_path`, `d.size_bytes`, `d.left_bytes`, `d.completed_bytes`, `d.complete`, `d.state`, counters, ratio | Implemented from `SessionRegistry` state |
| Custom fields | `d.custom`, `d.custom.set` | Implemented as facade-local roundtrip state for labels and migration/client metadata |
| Multicall/views | `d.multicall`, `d.multicall2`, `view.list`, `view.size` | Implemented with rTorrent row-array shape over the native registry |
| Loading | `load.normal`, `load.start`, `load.raw`, `load.raw_start` | Implemented for magnet URIs and filesystem `.torrent` paths; unsupported URL fetching stays a no-op placeholder for SSRF safety |
| Lifecycle | `d.erase`, `d.pause`, `d.resume`, `d.stop`, `d.start`, `d.tracker_announce` | Implemented as native engine hooks when attached plus registry erase projection |
| File/tracker/peer detail | `f.*`, `t.*`, `p.*` multicalls | Implemented as stable compatibility shapes; live detail remains a placeholder until native engine snapshots expose equivalent data |

## Cross-Client API Backlog

| Priority | Work item | Why |
|---|---|---|
| Done | Add tests that enumerate advertised facade methods and assert every advertised method returns a compatibility-shaped response | Prevents method-list drift |
| Done | Add `scripts/api_facade_certification.sh` as the deterministic pass/fail gate for qBit, Transmission, Deluge, and rTorrent facade matrices | Gives the compatibility docs one local certification entry point |
| Done | Add `scripts/universal_compatibility_certification.sh` as the broad local gate for facade, migration, native API, engine, and scale compatibility evidence | Gives release certification a wider deterministic local gate before live Docker/client runs |
| Done | Add cross-source import/apply matrices for JSON and bencoded resume aliases | Prevents fast-resume regressions when migrating from old clients |
| Done | Expand qBittorrent preference response breadth and API-key compatibility routes | Settings panes and modern API clients probe these before mutation |
| Done | Persist qBittorrent mutable preferences | Avoids settings panes seeing accepted writes disappear |
| Done | Deepen qBittorrent torrent property field values | Avoids detail panes seeing compatibility placeholders for values derivable from registry/engine state |
| Done | Add minimal rTorrent XMLRPC facade with command enumeration and request/response fixtures | Removes rTorrent facade as a universal compatibility gap while documenting placeholder depth |
| Medium | Replace synthetic import aliases with real exported golden corpora for every supported legacy client version family | Catches undocumented key variants and nested plugin state |
| Done | Preserve scheduler/RSS/search/autoadd/blocklist/execute/plugin metadata as auxiliary migration artifacts | Useful for full client migration; intentionally separate from torrent progress |
| Low | Proprietary client deep parsers for Tixati/BiglyBT plugin-only fields | Needs fixture corpus; progress import already uses generic verified paths |
