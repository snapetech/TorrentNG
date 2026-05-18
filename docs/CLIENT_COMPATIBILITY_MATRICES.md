# Client Compatibility Matrices

This file is the source-of-truth backlog for TorrentNG's universal
compatibility goal: move into TorrentNG from other clients, move out with
predictable state, expose the APIs existing tools already speak, and interoperate
with other BitTorrent clients on the wire.

The goal is intentionally broad enough to include qBittorrent, Transmission,
Deluge, rTorrent, uTorrent/BitTorrent Classic, BiglyBT/Vuze, Tixati, generic
`.torrent` directories, common automation clients, and real public/private swarm
behavior. A row is not considered complete because an endpoint exists; it is
complete when behavior, state projection, import/export or migration semantics,
and certification coverage are all documented.

This matrix separates the product target from current implementation status. The
target is universal in/out compatibility. The status column says how close the
current code is today.

Primary references checked on 2026-05-17:

- qBittorrent WebUI API 5.0:
  https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29
- Transmission RPC specification:
  https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md
- Deluge Web JSON-RPC API:
  https://deluge.readthedocs.io/en/deluge-2.0.5/reference/webapi.html
- Deluge core RPC API:
  https://deluge.readthedocs.io/en/deluge-2.0.4/reference/api.html
- rTorrent command reference:
  https://kannibalox.github.io/rtorrent-docs/cmd-ref.html

Status legend:

| Status | Meaning |
|---|---|
| Native | Backed by TorrentNG engine/session behavior |
| Compat | API shape is accepted and returns a client-compatible result; may be no-op if the feature has no native equivalent |
| Partial | Useful behavior exists, but fields, persistence, filtering, or error shape is incomplete |
| Gap | Not implemented |
| Test gap | Behavior exists, but matrix/certification coverage is missing or too shallow |

Universal compatibility release rule:

- Every P0/P1 row must be either `Native` or explicitly documented as
  compatibility-only with a reason.
- Every compatibility-only row must state whether it is a safe no-op, a
  placeholder projection, or a deliberate non-goal.
- Every import path must have dry-run reporting, path remapping behavior, and a
  corpus or fixture proving preserved fields.
- Every API facade must have endpoint/method enumeration, field-shape tests, and
  at least one live client or automation-flow certification row.
- Every wire-level claim must be backed by interop evidence against at least one
  independent client.

## 1. Feature Matrix

| Capability | qBittorrent | Transmission | Deluge | rTorrent | TorrentNG status | Required certification rows |
|---|---|---|---|---|---|---|
| Add `.torrent` | Web API multipart | `torrent_add` metainfo | `core.add_torrent_file`, `web.add_torrents` | `load.*` commands | Native through qBit, Transmission, Deluge facades; rTorrent XMLRPC accepts path/raw load shapes and projects registry state | Add file via every facade; verify native list and payload |
| Add magnet | `torrents/add urls=magnet` | `torrent_add filename=magnet` | `core.add_torrent_magnet` | `load.normal` magnet-capable builds/scripts | Native through qBit, Transmission, Deluge facades; rTorrent XMLRPC accepts magnet load | Magnet with tracker, magnet metadata fetch, DHT-only magnet |
| Pause/resume/start/stop | pause/resume plus v5 start/stop | start/start_now/stop | pause/resume | `d.stop`, `d.start` | Native where engine attached; registry fallback for reads | Per-facade lifecycle transition assertions |
| Remove torrent | delete with optional data | torrent_remove | core.remove_torrent | `d.erase` | Native through qBit/Transmission/Deluge | Remove torrent-only and remove-with-data rows |
| Force recheck | recheck | torrent_verify | force_recheck | `d.check_hash` | Native facade hooks | Corrupt data, force recheck, redownload repair |
| Reannounce | reannounce | torrent_reannounce | tracker update/force reannounce indirectly | `d.tracker_announce` | Native qBit/Transmission | HTTP/UDP tracker announce row |
| Move storage | setLocation/setSavePath | torrent_set_location | move_storage | directory/base path commands | Native where engine attached; registry fallback | Move path during stopped and active torrent |
| Rename torrent/file/folder | rename/renameFile/renameFolder | torrent_rename_path | rename_files/rename_folder | path custom commands/plugins | Native for file/folder where engine supports | Rename file/folder and verify metadata projection |
| File priority/wanted | filePrio | file priority/wanted calls | set_torrent_file_priorities | priority commands/plugins | Native facade hooks | Partial file selection row across facades |
| Queue ordering | top/bottom/increase/decrease | queue_move_* | queue_top/up/down/bottom | priority views | Native facade hooks | Queue mutation plus list-order projection |
| Categories/labels/tags | categories, tags | labels | Label plugin | custom fields | Native category/tags model | Label/category import and API mutation rows |
| Trackers | list/add/edit/remove | tracker list mutation, tracker_stats | set_torrent_trackers | tracker commands | Native metadata mutation; stats partially projected | Tracker add/edit/remove and stats row |
| Peers | addPeers, torrentPeers | peers fields | connect_peer | peer commands | Add/connect peer hooks; peer projection partial | Explicit peer private torrent row |
| Web seeds | webseeds read | webseeds/webseeds_ex | file/web seed via libtorrent state | supported through metainfo | Read projection implemented; Transmission activity fields project native webseed snapshots when an engine is attached | Webseed-only transfer and webseed projection row |
| Global speed limits | transfer limits | session limits | config/options speed limits | throttle commands | Native global limits through qBit/Transmission; Deluge compat | Set/read speed limits through each facade |
| Per-torrent speed limits | torrent limit endpoints | torrent_set limits | set_torrent_options | throttle commands | Native/Compat through qBit, Transmission, and Deluge facades; rTorrent throttle commands are compatibility placeholders | Per-torrent limit mutation and projection row |
| Sequential/first-last | qBit toggles | 4.1 sequential fields | options/prioritize first-last | client-specific | Mutation/projection implemented where facades expose the fields; native scheduler effects remain per-client depth | Assert accepted; add native support if scheduler implements |
| Super seeding | qBit setSuperSeeding | seed mode fields | super_seeding option | supported in rTorrent | Mutation/projection implemented as torrent limit state; native choking strategy effects remain later | API acceptance plus projection row |
| RSS | qBit RSS API | none core | plugin ecosystem | ruTorrent plugins | qBit stateful in-memory compatibility for folders, feeds, and rules | RSS API shape and mutation tests |
| Search | qBit search API | none core | plugin ecosystem | ruTorrent plugins | qBit stateful plugin/job compatibility with inert local result sets | Search API shape and lifecycle tests |
| Logs | qBit log endpoints | none core | events | log files | qBit main log backed by retained app/session events and peer log projects native peer snapshots when an engine is attached; Deluge events basic | Log/event shape tests |
| Session stats | transfer/info, sync | session_stats | session_status/stats | global commands | Native counters; qBit, Transmission, and Deluge session rates aggregate native peer snapshots where available | Cross-facade stats consistency row |
| Auth/session handshake | cookie login | CSRF/session id plus JSON-RPC 2.0 | JSON-RPC auth.login | external HTTP auth | Compat | Auth handshake rows for every facade |

## 2. Import And Fast-Resume Matrix

| Source client | Expected state sources | TorrentNG import entry point | Fields to preserve | Current status | Tests required |
|---|---|---|---|---|---|
| qBittorrent | `.torrent`, `.fastresume`, libtorrent resume keys | `dry_run_qbittorrent_backup_with_options` | info hash, name, save path, category, tags, added/completed times, paused/completed state, uploaded/downloaded, piece states, partial pieces, file priority/wanted/completed bytes, trackers | Native import implemented; JSON and bencoded alias matrix covered | Golden qBit profile with all fields; path remap; conflicting piece count |
| rTorrent | session `.torrent`, `.rtorrent` resume sidecars, file layout, `d.custom*` | `dry_run_rtorrent_session_with_options` | info hash, name, directory/save path, `d.custom1` label, counters, completed state; complete-state piece synthesis when files are present | Native import implemented; complete torrents synthesise trusted seed state (recheck-free), partials verification-first (sidecar has no decodable partial bitfield) | `round_trip_matrix.rs` rTorrent complete/partial; session fixture with `d.custom*`, missing/resized files |
| Transmission | `.torrent`, resume sidecars, legacy and 4.x key names | `dry_run_transmission_session_with_options` | download dir, labels, counters, timestamps, paused/completed, bitfield/have/valid pieces, file wanted/priority/progress, tracker stats | Native import implemented; JSON and bencoded alias matrix covered | Transmission profile with old and new key spellings |
| Deluge | `state`, `torrents.state`, `.fastresume`, libtorrent resume | `dry_run_deluge_state_with_options` | download location, label, counters, timestamps, paused/completed, pieces, file wanted/priority/progress, trackers | Native import implemented; JSON and bencoded alias matrix covered | Deluge state fixture plus JSON-RPC field projection comparison |
| uTorrent/BitTorrent Classic | `resume.dat`, `.dat` bencode, raw hash keys | `dry_run_utorrent_config_with_options` | path/rootdir, labels, counters, timestamps, state flags, pieces/bitfield, file priorities | Native import implemented; aggregate and bencoded alias matrix covered | `resume.dat` corpus with single/multi-file and skipped files |
| BiglyBT/Vuze | `downloads.config`, `torrents.config`, nested maps | `dry_run_biglybt_config_with_options` | hex hash keys, save path, categories, tags, counters, timestamps, file progress, tracker activity | Native for common nested/sidecar resume data; bencoded alias matrix covered | BiglyBT fixture corpus with plugin category fields |
| Tixati | config directory, `.torrent`, sidecar-like state | `dry_run_tixati_config_with_options` | metadata, path hints, counters/timestamps/progress if discoverable | Verification-first import; common bencoded alias matrix covered | Tixati fixture corpus; document unknown/private fields |
| Generic directory | `.torrent` plus adjacent JSON/bencode sidecars | `dry_run_generic_torrent_directory_with_options` | metadata, path remaps, sidecar resume hints | Native; aggregate `resume.dat` detection and JSON/bencoded alias matrices covered | Recursive fixture with symlink, oversized sidecar, path remap |

### Reverse export matrix (`torrentngd export` / `rt_migrate::export`)

Export reads native state read-only (DB rows + persisted `.torrent` blobs +
fast-resume) and writes the target client's on-disk layout. Fidelity is
reported per torrent as recheck-free / complete-only / metadata-only /
torrent-only.

| Target | `--format` | Layout written | Fidelity | Round-trip test |
|---|---|---|---|---|
| qBittorrent / Deluge | `libtorrent` | `<hash>.torrent` + `<hash>.fastresume` | Recheck-free, partials included | `round_trip_matrix.rs` |
| Transmission | `transmission` | `torrents/` + `resume/` | Recheck-free, partials included | `round_trip_matrix.rs` |
| rTorrent | `rtorrent` | `<hash>.torrent` + `<hash>.rtorrent` | Recheck-free for complete; partials metadata-only | `round_trip_matrix.rs` |
| uTorrent/BitTorrent Classic | `utorrent` | aggregate `resume.dat` | Recheck-free, whole-piece granularity | `round_trip_matrix.rs` |
| BiglyBT/Vuze | `biglybt` | aggregate `downloads.config` | Recheck-free, whole-piece granularity | `round_trip_matrix.rs` |
| Tixati | _(none)_ | use `generic` | Proprietary progress format has no encoder | covered via generic |
| Generic / any | `generic` | `<hash>.torrent` files + `manifest.json` | Torrent-only; destination rechecks | `round_trip_matrix.rs` |

Certification: `crates/rt-migrate/tests/round_trip_matrix.rs` exercises every
client across IMPORT, EXPORT, and ROUND-TRIP (export → re-import) directions
with ISO-shaped single-file and multi-file fixtures in both complete and
partial state, and is reported by
`scripts/migration_corpus_certification.sh` under "Bidirectional Round-Trip
Matrix".

## 3. qBittorrent API Matrix

Local implementation: `crates/rt-api-qbit`.

| Group | Upstream API points | TorrentNG points | Status | Test rows |
|---|---|---|---|---|
| Auth | `auth/login`, `auth/logout` | Same | Compat | Login/logout status and cookie shape |
| App info | `app/version`, `webapiVersion`, `buildInfo`, `preferences`, `setPreferences`, `shutdown`, `sendTestEmail`, `getCookies`, `setCookies`, `rotateAPIKey`, `deleteAPIKey`, `networkInterfaceList`, `networkInterfaceAddressList`, `defaultSavePath` | Same | Native/Compat mix; preferences, cookies, and API-key lifecycle round-trip in facade state | Probe every endpoint plus preference/cookie/API-key persistence |
| Torrent list/add | `torrents/info`, `torrents/add` | Same | Native | Add magnet/file, list filters/sort/category/tag/hash |
| Torrent lifecycle | `pause`, `resume`, `start`, `stop`, `delete`, `recheck`, `reannounce` | Same | Native | Lifecycle transition per endpoint; qBittorrent list state projects active recheck jobs as checking |
| Torrent trackers/peers | `trackers`, `addTrackers`, `editTracker`, `removeTrackers`, `addPeers` | Same | Native/Partial stats; persisted engine tracker rows project status, message, and scrape counts where available | Tracker mutation and explicit peer row |
| Torrent files/pieces | `files`, `webseeds`, `pieceStates`, `pieceHashes`, `export`, `filePrio` | Same | Native/Partial | File priority and piece state are engine-backed; `export` returns the persisted raw `.torrent` bytes for known torrents with stored metadata |
| Queue priority | `increasePrio`, `decreasePrio`, `topPrio`, `bottomPrio` | Same | Native | Queue ordering row |
| Properties | `properties` | Same | Partial projection | Full property key presence row |
| Categories | `categories`, `createCategory`, `editCategory`, `removeCategories`, `setCategory` | Same | Native/Compat; configured category save paths apply on set | Category create/edit/remove/set row |
| Tags | `tags`, `createTags`, `deleteTags`, `addTags`, `setTags`, `removeTags` | Same | Native/Compat; global tags persist and clean up when unused | Tags global and per-torrent row |
| Limits/modes | `downloadLimit`, `setDownloadLimit`, `uploadLimit`, `setUploadLimit`, `setShareLimits`, `setForceStart`, `setSuperSeeding`, `setAutoTMM`, `setAutoManagement`, `toggleSequentialDownload`, `toggleFirstLastPiecePrio` | Same | Partial/Compat | Limit read/write, mode accepted, native behavior later |
| Sync | `sync/maindata`, `sync/torrentPeers` | Same | Native/Partial peers; maindata includes broad torrent/server-state keys, torrentPeers has qBit peer shape and stable RID deltas | Full sync, delta sync, peer sync row |
| Transfer | `transfer/info`, download/upload limits, speed limits mode, toggle, setters, `banPeers` | Same | Native/Compat | Global limit and ban accepted rows |
| Logs | `log/main`, `log/peers` | Same | Native/Compat | Main log projects retained native session events, sidecar app events, and optional ingested rTorrent logs with qBit severity filters; peer log projects bounded native peer snapshots when an engine is attached |
| Search | status/categories/plugins/install/uninstall/enable/update/start/stop/results/delete | Same | Compat | Plugin install/enable/uninstall plus job start/results/delete lifecycle |
| RSS | items/rules/matchingArticles/addFolder/addFeed/removeItem/moveItem/markAsRead/refreshItem/setRule/renameRule/removeRule | Same | Compat | Folder/feed and rule mutation round-trip |

qBittorrent field backlog:

| Surface | Fields to audit exhaustively | Current risk |
|---|---|---|
| `app/preferences` | Broad current/legacy WebUI preference key set across paths, queueing, BitTorrent, WebUI, RSS, proxy, and advanced settings | Implemented compatibility defaults plus in-memory `setPreferences` persistence for arbitrary qBit preference keys |
| `torrents/info` | Core list fields plus modern path, session counter, lifecycle, limit, mode, magnet, infohash, and native-backed live swarm counters/rates | Implemented compatibility breadth for common remote-app columns |
| `torrents/properties` | Full properties object | Implemented key set with registry/engine-backed counters, lifecycle times, piece counts, and per-torrent limits where available |
| `sync/maindata` / `sync/torrentPeers` | Server state, categories, tags, torrents, trackers, peers | Broad torrent/server-state key sets and peer shape/RID stability are matrix-tested; remaining risk is deeper live tracker delta fidelity |

## 4. Transmission RPC Matrix

Local implementation: `crates/rt-api-transmission`. TorrentNG accepts old
kebab/camel calls and normalizes Transmission 4.1 snake_case calls.

| Method group | Upstream methods | Local status | Test rows |
|---|---|---|---|
| JSON-RPC shape | JSON-RPC 2.0, snake_case names; old bespoke RPC deprecated but still common | Compat: JSON-RPC 2.0 single and batch requests, `params`, direct `result`, error object, snake_case names/keys; old envelope remains supported | JSON-RPC 2.0 envelope row, batch row, old envelope row, CSRF header row |
| Torrent accessor | `torrent_get` with `objects` and `table` formats, `recently_active` removed list | Compat: objects, table rows, and empty removed list supported | All-field object row; table format row; recently-active row |
| Torrent mutator | `torrent_set` | Compat/native mix; labels, location, speed limits, peer limits, seed limits, and sequential mode project after mutation | Per-field mutation acceptance and projection |
| Torrent add | `torrent_add` | Native for magnet/metainfo/download dir/paused/labels | Magnet, metainfo, duplicate, invalid metainfo rows |
| Torrent actions | `torrent_start`, `torrent_start_now`, `torrent_stop`, `torrent_verify`, `torrent_reannounce`, `torrent_remove` | Native | Lifecycle action rows |
| Torrent location/rename | `torrent_set_location`, `torrent_rename_path` | Native/Partial | Move and rename row |
| File controls | `torrent_set_file_priorities`, `torrent_set_file_wanted`, `torrent_set_file_unwanted` | Native | File selection row |
| Trackers | `torrent_set_tracker_list` | Native | Tracker list row |
| Queue | `queue_move_top`, `queue_move_up`, `queue_move_down`, `queue_move_bottom` | Native | Queue row |
| Session | `session_get`, `session_set`, `session_stats`, `session_close`, `session_access_control`, `session_subscribe`, `session_unsubscribe` | Compat/native mix; `session_get` field projection supported and broad mutable settings plus notification subscriptions roundtrip in facade state | Session fields, mutable settings, and notification subscription row |
| Utilities | `blocklist_update`, `port_test`, `free_space` | Compat | Utility shape row |
| Groups | `group_get`, `group_set`, torrent group assignment | Compat: group limit state and per-torrent group projection roundtrip for remote-client parity; native scheduler effects remain future work | Group shape, mutation, and torrent projection row |

Transmission `torrent_get` field matrix:

| Field bucket | Upstream fields | TorrentNG status |
|---|---|---|
| Identity | `id`, `hash_string`, `name`, `magnet_link`, `metadata_percent_complete`, `is_private` | Native/Compat |
| Size/progress | `total_size`, `left_until_done`, `percent_complete`, `percent_done`, `size_when_done`, `have_valid`, `have_unchecked`, `desired_available`, `bytes_completed`, `availability`, `pieces`, `piece_count`, `piece_size` | Native/partial: byte counters, piece counts, availability, valid/unchecked/desired availability, and compact `pieces` bitfield use registry and native piece state where available |
| State/dates | `status`, `error`, `error_string`, `eta`, `eta_idle`, `is_finished`, `is_stalled`, `recheck_progress`, `activity_date`, `added_date`, `done_date`, `start_date`, `date_created`, `seconds_downloading`, `seconds_seeding` | Native/Partial; ETA projects from native peer rates where available and active recheck jobs project `recheck_progress` |
| Counters/ratio | `downloaded_ever`, `uploaded_ever`, `upload_ratio`, `corrupt_ever` | Native/Compat |
| Rates/limits | `rate_download`, `rate_upload`, `download_limit`, `download_limited`, `upload_limit`, `upload_limited`, `bandwidth_priority`, `honors_session_limits`, `max_connected_peers` | Native/compat for aggregate peer rates and limit mutation/projection; bandwidth priority and session-limit flags remain compatibility fields |
| Seed limits | `seed_ratio_limit`, `seed_ratio_mode`, `seed_idle_limit`, `seed_idle_mode` | Compat/native mutation and projection |
| Files | `files`, `file_stats`, `priorities`, `wanted` | Native/Partial |
| Peers | `peers`, `peers_connected`, `peers_from`, `peers_getting_from_us`, `peers_sending_to_us` | Partial; `peers_from` shape implemented with best-effort counts |
| Trackers | `trackers`, `tracker_stats` including announce/scrape states and counts | Native/Partial; persisted announce status, timestamps, failure/warning messages, and scrape counts project from engine tracker rows |
| Web seeds | `webseeds`, `webseeds_sending_to_us`, `webseeds_ex` | Native/Partial; URL list projects from metainfo and activity/rate fields project native webseed snapshots when available |
| Queue/group | `queue_position`, `group` | Partial; default group compatibility implemented |
| Comments/creator | `comment`, `creator`, `primary_mime_type` | Native/Partial; torrent comment, creator, creation date, and primary MIME type project from parsed metainfo where available |
| Sequential | `sequential_download`, `sequential_download_from_piece` | Sequential flag and from-piece mutation/projection roundtrip implemented; picker-level behavioral effects remain native work |

Transmission `session_get` field matrix:

| Bucket | Upstream fields | TorrentNG status |
|---|---|---|
| Version/protocol | `version`, `rpc_version`, `rpc_version_minimum`, `rpc_version_semver`, `session_id`, `units` | Compat; semver is reported, session header supported |
| Paths/start behavior | `download_dir`, `incomplete_dir`, `incomplete_dir_enabled`, `rename_partial_files`, `start_added_torrents`, `trash_original_torrent_files` | Compat; mutable settings roundtrip in facade state |
| Speed limits | normal and alt speed fields, scheduler day/begin/end/enabled | Native global limits plus compat scheduler fields |
| Queue | download/seed queue, queue stalled settings | Compat; mutable settings roundtrip in facade state |
| Peer/network | peer limits, peer port, port forwarding, DHT/PEX/LPD/uTP, preferred transports | Compat; mutable settings roundtrip in facade state |
| RPC/security | auth, whitelist, bind address, anti brute force, username | Compat placeholder |
| Blocklist | enabled, size, URL | Compat; enabled/URL/size roundtrip plus update response projection |
| Scripts | added/done/done-seeding script paths/enabled | Compat; mutable settings roundtrip in facade state |
| Seeding | ratio and idle limits | Compat; mutable settings roundtrip in facade state |

## 5. Deluge API Matrix

Local implementation: `crates/rt-api-deluge`.

| Method group | Upstream methods | Local status | Test rows |
|---|---|---|---|
| JSON endpoint/auth | `/json`, `auth.login`, `auth.check_session` | Compat | Auth row |
| Daemon | `daemon.login`, `daemon.info`, `daemon.get_method_list`, `daemon.shutdown` | Compat | Method-list parity row |
| Web host management | `web.add_host`, `edit_host`, `remove_host`, `get_hosts`, `get_host_status`, `connect`, `disconnect`, `connected`, `start_daemon`, `stop_daemon` | Compat/native shape implemented | Host management shape row |
| Web torrent helpers | `web.add_torrents`, `download_torrent_from_url`, `get_torrent_files`, `update_ui`, `get_events` | Compat/native shape implemented; `web.add_torrents` accepts magnet, temp-file path, embedded metainfo/base64, and URL placeholder payloads; `update_ui` honors requested fields and emits filter/stat shape; URL fetch intentionally no-op to avoid server-side fetch | Web add and update row |
| Web config/plugins | `web.get_config`, `update_config`, `save_config`, plugins | Compat implemented | Web config row |
| Core session | `core.get_session_status`, stats/rates/connections, filter tree, cache status, config values | Native/Compat; rates aggregate native peer snapshots when available | Session/status/config rows |
| Core torrent reads | `get_torrents_status`, `get_torrent_status`, `get_torrent_file_status`, `get_session_state` | Native/Compat; requested-key filtering, label/state/hash filters, and option projection implemented | Requested-key/filter row |
| Core lifecycle | add file/magnet, pause/resume, force_recheck, remove | Native | Lifecycle rows |
| Core mutation | set options, priorities, trackers, queue, move, rename, connect_peer | Native/Compat; torrent options roundtrip in facade state and apply to engine when available | Mutation rows |
| Label plugin | label list/add/remove/options/set_torrent | Native/Compat | Label plugin row |
| Notifications plugin | handled events, subscriptions, config/add subscription | Compat | Notification shape row |
| Other plugins | AutoAdd, Blocklist, Execute, Extractor, Scheduler | Migration artifact preservation implemented; facade APIs expose stateful compatibility config/command surfaces while native behavioral effects remain explicit non-goals unless TorrentNG owns that workflow | Artifact preservation and plugin API rows |

Deluge torrent status field matrix:

| Field bucket | Common Deluge fields | TorrentNG status |
|---|---|---|
| Identity/path | `hash`, `name`, `save_path`, `label`, `owner`, `shared` | Native/Compat |
| Progress/size | `progress`, `total_size`, `total_done`, `num_files`, `num_pieces`, `piece_length` | Native/Partial |
| State/time | `state`, `is_finished`, `eta`, `time_added`, `completed_time`, `active_time`, `seeding_time`, `finished_time` | Compat/native; ETA projects from native peer rates where available and active recheck jobs project `Checking` state |
| Rates/counters | download/upload rates, total payload download/upload, all-time download, ratio | Native/compat: rates project native peer snapshots when available; counters project registry stats |
| Peers/seeds | `num_peers`, `num_seeds`, `total_peers`, `total_seeds`, distributed copies | Native/compat from peer snapshots when an engine is attached |
| Trackers | `tracker`, `tracker_host`, `tracker_status`, `next_announce` | Native/Partial; persisted engine tracker rows project URL, host, status/warning/error text, and next announce where available |
| Options | max speeds, auto managed, stop ratio, move completed, sequential, super seeding, first/last | Compat/native for speed, auto-managed, stop ratio, move-completed, sequential, super seeding, first/last |
| Messages | `comment`, `message`, `private` | Compat/native for torrent comments, error message, and private flag |

## 6. rTorrent XMLRPC Matrix

Local implementation: `crates/rt-api-rtorrent`.

TorrentNG exposes a minimal rTorrent XMLRPC compatibility dispatcher for clients
and migration/certification probes that expect rTorrent-shaped commands. The v1
scope is intentionally compatibility-shaped: registry-backed torrent identity,
progress, custom fields, lifecycle hooks when an engine is attached, registry
fallback file rows, and native-backed file/tracker/peer multicall projections
when engine metadata or peer snapshots are available.

| Command family | Upstream examples | TorrentNG status | Test rows |
|---|---|---|---|
| System/session | `system.*`, `session.*`, `network.*`, throttle commands | Native/Compat: version/session/network values plus native global throttle reads when an engine is attached | Method enumeration and XMLRPC fixture rows |
| Download/torrent | `d.*`, `d.multicall*`, `load.*` | Compat/native mix: registry-backed reads, custom field roundtrip, magnet/path load, lifecycle hooks | Read projection, custom field, multicall, load/erase rows |
| File | `f.*` | Native/compat: registry fallback row without an engine; native metadata rows expose path, size, offsets, piece ranges, priority, and completion | File multicall shape and projection rows |
| Tracker | `t.*`, tracker announce controls | Native/Compat: tracker rows expose native announce URLs, and persisted engine tracker status/scrape counters when available; announce accepted and engine tracker work remains covered by interop matrix | Tracker multicall and announce acceptance row |
| Peer | `p.*` | Native/compat: peer rows expose native peer address, port, client, progress, rates, choke/interested flags when an engine is attached | Peer multicall projection row |
| Views/queue | `view.*`, priority/custom views | Native/Compat: registry-backed built-in view sizes plus stateful custom `view.add`/`view.set` list and size projection | View list/size row |

## 7. Test Matrix Backlog

| Priority | Test artifact | Coverage |
|---|---|---|
| P0 | `api_facade_endpoint_matrix` | Implemented in crate tests, `scripts/api_facade_certification.sh`, and `scripts/universal_compatibility_certification.sh`: qBit route matrix, Transmission method matrix, Deluge advertised method matrix, and rTorrent XMLRPC method matrix |
| P0 | `api_response_field_matrix` | Implemented in crate tests, `scripts/api_facade_certification.sh`, and `scripts/universal_compatibility_certification.sh` for current qBit `torrents/info`/`properties`/`sync`, Transmission `torrent-get`/`session-get`, Deluge torrent status fields, and representative rTorrent XMLRPC fixtures |
| P0 | `import_fixture_matrix` | Implemented for common JSON resume fields and source-specific bencoded aliases across qBit, Transmission, Deluge, uTorrent, BiglyBT/Vuze, Tixati, and Generic; reverse export round-trips through qBit/libtorrent, Transmission, rTorrent, uTorrent, BiglyBT, and Generic importers; exported-corpus gate is wired through `scripts/migration_corpus_certification.sh` and reports `PASS_WITH_GAPS` until real artifacts are populated |
| P0 | `migration_apply_matrix` | Implemented in `rt-migrate` tests and `scripts/universal_compatibility_certification.sh` for common JSON and bencoded resume fields across qBit, Transmission, Deluge, uTorrent, BiglyBT/Vuze, Tixati, and Generic: applies DB rows and fastresume, reloads, and asserts preservation |
| P1 | `qbit_arr_client_matrix` | Sonarr/Radarr/Prowlarr/cross-seed/autobrr-style qBit flows are covered by Track 1 sidecar tests, `scripts/configure_certification_clients.sh`, `scripts/arr_app_certification.sh`, `scripts/app_add_job_certification.sh`, `scripts/autobrr_certification.sh`, and `scripts/release_grab_certification.sh`; NZB360/Transdrone-style qBit read flows are covered by `scripts/mobile_compat_certification.sh` and can be included in the universal gate with `UNIVERSAL_COMPAT_MOBILE=1` |
| P1 | `transmission_client_matrix` | transmission-web/transmission-remote field projection, mutable session settings, queue/file/tracker/torrent actions, JSON-RPC 2.0 error shape, and stateful notification subscription probes are covered by `rt-api-transmission` tests and `scripts/api_facade_certification.sh`; live mobile-style qBit read compatibility is covered by `scripts/mobile_compat_certification.sh` |
| P1 | `deluge_client_matrix` | Deluge WebUI `update_ui`, thin-client core calls, add flows, Label plugin calls, and file priority actions are covered by `rt-api-deluge` tests and `scripts/api_facade_certification.sh` |
| P1 | `interop_transfer_matrix` | qBit/Transmission/Deluge/rTorrent seed and leech with TorrentNG both directions; the Docker protocol row `rust-seeds-to-all-reference-clients` covers TorrentNG as the sole seeder for all reference clients in one swarm |
| P1 | `tracker_peer_matrix` | HTTP tracker, UDP tracker, private torrent DHT/PEX policy, explicit peer, multi-tracker fallback, tracker outage after peer discovery, and multi-peer completion are covered by Docker protocol rows in `scripts/interop_matrix.sh` |
| P1 | `storage_resume_matrix` | stop mid-transfer/restart/resume, corrupt block recheck repair, and missing file recovery are covered by Docker protocol rows `resume-after-partial-download`, `force-recheck-corruption-repair`, and `missing-file-recovery`; local storage topology coverage is included in `scripts/universal_compatibility_certification.sh` |
| P2 | `plugin_aux_matrix` | Migration artifact preservation implemented for RSS/search/scheduler/autoadd/blocklist/execute/plugin/config files; Deluge auxiliary plugin facades round-trip autoadd, blocklist, execute, extractor, scheduler, and enabled-plugin state; remaining work is native behavioral effects only where explicitly supported |
| P2 | `scale_matrix` | 15k imported torrents, hundreds active, many files, hostile paths; local scale coverage is included in `scripts/universal_compatibility_certification.sh` |

## 8. Build Backlog From Matrices

| Priority | Work item | Source matrix |
|---|---|---|
| P0 | Add automated endpoint/method enumeration tests for qBit, Transmission, and Deluge | Implemented in `rt-api-qbit`, `rt-api-transmission`, and `rt-api-deluge` unit tests |
| P0 | Add all-field response tests for qBit `torrents/info`, `properties`, `sync/maindata`; Transmission `torrent_get` and `session_get`; Deluge torrent status | Implemented in facade unit tests for currently supported fields |
| Done | Persist broad Transmission mutable session settings in facade state | Transmission API matrix |
| Done | Deepen Transmission 4.1 native parity beyond compatibility envelope: JSON-RPC 2.0 method errors, stateful notification subscription probes, and broad mutable session effects are covered in the Transmission API matrix; true push notification delivery remains future live-client work |
| Done | Deepen Deluge `web.add_torrents` with common WebUI magnet, embedded metainfo, temp-file path, and URL-placeholder payload shapes | Deluge API matrix |
| Done | Persist qBittorrent mutable preferences for arbitrary `setPreferences` keys | qBit field backlog |
| Done | Broaden qBittorrent property projections to documented keys backed by registry/engine state | qBit field backlog |
| P1 | Populate `testdata/migration-corpus/` with real exported golden fixtures for qBit, Transmission, Deluge, uTorrent, BiglyBT/Vuze, Tixati, rTorrent, and generic edge cases; enforce with `TNG_REQUIRE_MIGRATION_CORPUS=1` | Import matrix |
| Done | Add minimal rTorrent XMLRPC compatibility dispatcher with command enumeration and representative fixtures | rTorrent matrix |
| Done | Preserve auxiliary RSS/search/scheduler/autoadd/blocklist/execute/plugin/config metadata as migration artifacts | Feature/import matrices |
