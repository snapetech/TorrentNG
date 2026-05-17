# TorrentNG API Reference

TorrentNG exposes the same client-facing API families in both native-engine
mode and the Track 1 rTorrent sidecar:

- **Native API** — `/api/v1/` — JSON REST, snake_case, designed for the WebUI and direct integrations. In native-engine mode this is backed by durable engine state; in Track 1 it is a sidecar facade over rTorrent.
- **qBittorrent compat API** — `/api/v2/` and `/api/qb/v2/` — targeted as a drop-in replacement for the qBittorrent Web API v2; used by Prowlarr, Sonarr, Radarr, autobrr, cross-seed, etc.
- **Transmission RPC** — `/transmission/rpc` and `/api/transmission/rpc` — compatibility facade over the same torrent registry.
- **Deluge RPC** — Deluge-compatible facade for clients that expect Deluge method names, with parity tracked in the compatibility matrix.

All surfaces are served on the same port (default `8080`).

The API strategy is compatibility-first: existing tools should be able to keep
speaking the client dialect they already support while TorrentNG projects those
calls onto one native model. Endpoint availability does not by itself mean full
semantic parity; current native, partial, compatibility-only, and gap status is
tracked in [CLIENT_COMPATIBILITY_MATRICES.md](CLIENT_COMPATIBILITY_MATRICES.md).

For engine selection and native-vs-rTorrent behavior, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

---

## Authentication

When `auth.api_tokens` is configured, all non-public endpoints require one of:

```
Authorization: Bearer <token>
Cookie: tng_session=<token>
```

Public endpoints (never require auth): `/health`, `/metrics`, `/api/qb/v2/auth/login`

---

## Health And Capability Manifest

`GET /health` reports native-engine readiness and a machine-readable capability
manifest. Existing readiness fields remain stable (`status`, `ready`,
`native_engine`, `torrent_count`), and the nested `engine.capabilities` object
advertises rewrite-level support for v1/v2/hybrid identity, `btih`/`btmh`
magnets, durable session/job state, storage safety, DHT/uTP policy, native REST,
qBittorrent, Transmission, Deluge, migration, metrics, and diagnostics.

The `engine.track1_sidecar_required` field is always `false` for native-engine
mode; Track 1 remains a migration/facade layer, not a runtime dependency for
the rewritten engine.

---

## Native API — `/api/v1`

### Torrents

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/torrents` | List torrents with filter/sort/page |
| `POST` | `/api/v1/torrents` | Add torrent (multipart: `torrent` file or `magnet` URL, `save_path`, `category`, `start`) |
| `GET`  | `/api/v1/torrents/:hash` | Get single torrent by hash |
| `PUT`  | `/api/v1/torrents/:hash` | Update torrent metadata (`{ save_path }`) |
| `DELETE` | `/api/v1/torrents/:hash` | Remove torrent (`?delete_files=true` to also delete data) |
| `POST` | `/api/v1/torrents/:hash/start` | Start torrent |
| `POST` | `/api/v1/torrents/:hash/stop` | Stop torrent |
| `POST` | `/api/v1/torrents/:hash/recheck` | Force hash check |
| `POST` | `/api/v1/torrents/:hash/reannounce` | Force tracker announce |
| `GET`  | `/api/v1/torrents/:hash/trackers` | List trackers |
| `PATCH` | `/api/v1/torrents/:hash/trackers` | Add/remove/edit trackers (`{ add: ["url"], remove: ["url"], edit: [{ orig_url, new_url }] }`) |
| `GET`  | `/api/v1/torrents/:hash/files` | List files |
| `PATCH` | `/api/v1/torrents/:hash/files` | Set file priorities (`{ files: [{index, priority}] }`) |
| `PUT`  | `/api/v1/torrents/:hash/category` | Set category (`{ category: "name" }`) |
| `POST` | `/api/v1/torrents/:hash/tags` | Add tags (`{ tags: ["a","b"] }`) |
| `DELETE` | `/api/v1/torrents/:hash/tags` | Remove tags (`{ tags: ["a"] }`) |

#### `GET /api/v1/torrents` query parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `filter` | string | Name substring match (case-insensitive) |
| `status` | string | `seeding` \| `downloading` \| `stopped` \| `checking` \| `error` |
| `category` | string | Exact category name |
| `tag` | string | Exact tag name |
| `sort` | string | `name` \| `size` \| `added` \| `ratio` \| `speed_down` \| `speed_up` \| `progress` |
| `dir` | string | `asc` \| `desc` |
| `limit` | int | Max rows (1–5000, default 200) |
| `offset` | int | Pagination offset |

Response: `{ total: int, torrents: TorrentRow[] }`

#### TorrentRow fields

```json
{
  "hash": "abc123...",
  "name": "Example.Torrent.Name",
  "size_bytes": 10737418240,
  "bytes_done": 10737418240,
  "down_rate": 0,
  "up_rate": 1048576,
  "up_total": 53687091200,
  "down_total": 10737418240,
  "ratio": 5000,
  "is_active": true,
  "is_open": true,
  "complete": true,
  "state": 1,
  "priority": 0,
  "category": "Movies",
  "base_path": "/data/downloads/Example.Torrent.Name",
  "directory": "/data/downloads",
  "creation_date": 1700000000,
  "timestamp_finished": 1700001000,
  "tracker_focus": 0,
  "peers_connected": 12,
  "peers_complete": 400,
  "message": "",
  "tracker_url": "https://tracker.example.com/announce",
  "tags": "hd,4k",
  "updated_at": 1700100000
}
```

Notes:
- `ratio` is integer × 1000 (5000 = ratio 5.0)
- `tags` is a comma-separated string of tag names
- `state`: 0=idle, 1=active, 2=checking, 3=error

### Categories

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/categories` | List all categories |
| `POST` | `/api/v1/categories` | Create or update category (`{ name, save_path }`) |
| `DELETE` | `/api/v1/categories/:name` | Delete category |

### Tags

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/tags` | List all tag names |
| `POST` | `/api/v1/tags` | Create tag (`{ name }`) |
| `DELETE` | `/api/v1/tags/:name` | Delete tag (also removes from all torrents) |

### Storage

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/storage` | List configured storage roots with total/used/free bytes, readonly status, and per-root errors |
| `GET` | `/api/v1/tracker-health` | Aggregate cached torrents by tracker URL with torrent/active/complete/error/peer counts |
| `GET` | `/api/v1/engine` | Runtime engine provenance, XMLRPC capability probes, rTorrent HTTP tracker-stack telemetry, and drift from the bundled engine profile |
| `GET` | `/api/v1/engine/commands` | Full XMLRPC command index exposed by the running rTorrent build |
| `POST` | `/api/v1/cross-seed` | Preview/apply cross-seed helper (`{ hashes, trackers, reannounce, dry_run }`) |

### Saved Views

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/saved-views` | List saved torrent filter/sort views |
| `POST` | `/api/v1/saved-views` | Create/update saved view (`{ id, name, params }`) |
| `DELETE` | `/api/v1/saved-views/:id` | Delete saved view |

### Ratio Groups

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/ratio-groups` | List configured ratio groups |
| `POST` | `/api/v1/ratio-groups` | Create/update ratio group (`{ name, ratio_limit, seeding_time_limit, category, tracker, enabled }`) |
| `POST` | `/api/v1/ratio-groups/:name` | Apply ratio group to matching cached torrents (`{ dry_run }`) |
| `DELETE` | `/api/v1/ratio-groups/:name` | Delete ratio group |

### Workflow Rules

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/workflows` | List workflow rules |
| `POST` | `/api/v1/workflows` | Create/update workflow rule for `completed`, `added`, or `category_changed` events |
| `POST` | `/api/v1/workflows/:id` | Run workflow rule (`{ dry_run }`); native actions and webhook POST actions execute, script actions require `[workflows].allow_scripts=true` and pass the configured directory allowlist |
| `DELETE` | `/api/v1/workflows/:id` | Delete workflow rule |
| `GET` | `/api/v1/workflow-runs` | List the most recent workflow run audit records, capped to the latest 200 |

### RSS Rules

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/rss-rules` | List RSS automation rules |
| `POST` | `/api/v1/rss-rules` | Create/update RSS rule (`{ id, name, enabled, feed_url, include, exclude, category, save_path, tags, start }`) |
| `POST` | `/api/v1/rss-rules/test` | Test a sample `{ title, link }` against configured rules |
| `POST` | `/api/v1/rss-rules/apply` | Preview or apply a sample `{ title, link, dry_run }`; real runs submit matched links with rule category/save path/start settings |
| `DELETE` | `/api/v1/rss-rules/:id` | Delete RSS rule |

### Bulk operations

```
POST /api/v1/bulk/:action
```

Actions: `start`, `stop`, `recheck`, `reannounce`, `set-category`, `set-location`

Body: `{ hashes: ["abc123", ...], dry_run: false }`

For `set-category`, include `category` (empty string clears it). For `set-location`,
include non-empty `save_path`.

Response: `{ applied: ["abc123"], errors: [], dry_run: false }`

Pass `dry_run: true` to preview what would be affected without making changes.

### Settings

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/settings/user-agent` | Get current rTorrent user-agent string |
| `PUT`  | `/api/v1/settings/user-agent` | Set user-agent (`{ user_agent: "..." }`) — takes effect immediately |

### Infrastructure

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/health` | Health check. Native mode returns the engine readiness/capability manifest; Track 1 reports sidecar/rTorrent reachability. |
| `GET`  | `/metrics` | Prometheus text format metrics |
| `GET`  | `/ws` | WebSocket — upgrade to receive live events |

#### WebSocket events

```json
{ "type": "torrent_updated", "hash": "abc123" }
{ "type": "torrent_removed", "hash": "abc123" }
{ "type": "stats", "upload_speed": 1048576, "download_speed": 524288 }
```

---

## qBittorrent Compat API — `/api/v2` or `/api/qb/v2`

Implements the qBittorrent Web API v2. By default it advertises qBittorrent `5.0.0` / Web API `2.11.0`; lab builds can override those API-facing values with the `TNG_QBITTORRENT_*` identity environment variables. Configure external tools to point at this server as if it were qBittorrent.

### Auth

| Method | Path | Notes |
|--------|------|-------|
| `POST` | `/api/qb/v2/auth/login` | In no-auth mode accepts any credentials; with `auth.api_tokens`, use an API token as username or password to receive an `tng_session` cookie |
| `POST` | `/api/qb/v2/auth/logout` | No-op |

### App

| Method | Path |
|--------|------|
| `GET` | `/api/qb/v2/app/version` |
| `GET` | `/api/qb/v2/app/webapiVersion` |
| `GET` | `/api/qb/v2/app/buildInfo` |
| `GET` | `/api/qb/v2/app/preferences` |
| `GET` | `/api/qb/v2/app/defaultSavePath` |
| `POST` | `/api/qb/v2/app/setPreferences` | Form: `json` preference object; unsupported keys are accepted and ignored |

### Torrents

| Method | Path | Notes |
|--------|------|-------|
| `GET`  | `/api/qb/v2/torrents/info` | Filter params: `filter`, `category`, `tag`, `sort`, `reverse`, `limit`, `offset` |
| `GET`  | `/api/qb/v2/torrents/properties` | Query: `hash`; returns cached torrent properties |
| `POST` | `/api/qb/v2/torrents/add` | Multipart: `urls`, `savepath`, `category`, `tags`, `paused`, `stopped`, `skip_checking`, `autoTMM`, `contentLayout`, `ratioLimit`, `seedingTimeLimit`, `torrents` (file) |
| `POST` | `/api/qb/v2/torrents/pause` / `/stop` | Form: `hashes` (pipe-separated or `all`) |
| `POST` | `/api/qb/v2/torrents/resume` / `/start` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/delete` | Form: `hashes`, `deleteFiles` |
| `POST` | `/api/qb/v2/torrents/recheck` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/reannounce` | Form: `hashes` |
| `GET`  | `/api/qb/v2/torrents/trackers` | Query: `hash` |
| `GET`  | `/api/qb/v2/torrents/webseeds` | Returns `[]` |
| `GET`  | `/api/qb/v2/torrents/files` | Query: `hash` |
| `GET`  | `/api/qb/v2/torrents/pieceStates` | Returns `[]` |
| `GET`  | `/api/qb/v2/torrents/pieceHashes` | Returns `[]` |
| `POST` | `/api/qb/v2/torrents/filePrio` | Form: `hash`, `id` (pipe-separated indices), `priority` |
| `POST` | `/api/qb/v2/torrents/setCategory` | Form: `hashes`, `category` |
| `POST` | `/api/qb/v2/torrents/addTags` | Form: `hashes`, `tags` (comma-separated) |
| `POST` | `/api/qb/v2/torrents/removeTags` | Form: `hashes`, `tags` |
| `POST` | `/api/qb/v2/torrents/setTags` | Form: `hashes`, `tags` (comma-separated replacement) |
| `GET`  | `/api/qb/v2/torrents/categories` | Returns `{ "Name": { "name": "Name", "savePath": "/path" } }` |
| `POST` | `/api/qb/v2/torrents/createCategory` | Form: `category`, `savePath` |
| `POST` | `/api/qb/v2/torrents/editCategory` | Form: `category`, `savePath` |
| `POST` | `/api/qb/v2/torrents/removeCategories` | Form: `categories` (newline-separated) |
| `GET`  | `/api/qb/v2/torrents/tags` | Returns `["tag1", "tag2"]` |
| `POST` | `/api/qb/v2/torrents/createTags` | Form: `tags` (comma-separated) |
| `POST` | `/api/qb/v2/torrents/deleteTags` | Form: `tags` |
| `POST` | `/api/qb/v2/torrents/rename` | Form: `hash`, `name` |
| `POST` | `/api/qb/v2/torrents/renameFile` | Form: `hash`, `id`, `name` |
| `POST` | `/api/qb/v2/torrents/renameFolder` | Form: `hash`, `id`, `name` |
| `GET`  | `/api/qb/v2/torrents/downloadLimit` | Returns `{}` |
| `POST` | `/api/qb/v2/torrents/setDownloadLimit` | Accepted |
| `GET`  | `/api/qb/v2/torrents/uploadLimit` | Returns `{}` |
| `POST` | `/api/qb/v2/torrents/setUploadLimit` | Accepted |
| `POST` | `/api/qb/v2/torrents/setShareLimits` | Form: `hashes`, `ratioLimit`, `seedingTimeLimit` |
| `POST` | `/api/qb/v2/torrents/setLocation` | Form: `hashes`, `location` |
| `POST` | `/api/qb/v2/torrents/setSavePath` | Form: `hashes`, `location` |
| `POST` | `/api/qb/v2/torrents/addTrackers` | Form: `hashes`, `urls` (newline-separated) |
| `POST` | `/api/qb/v2/torrents/setAutoTMM` | Accepted as a compatibility no-op |
| `POST` | `/api/qb/v2/torrents/editTracker` | Form: `hash`, `origUrl`, `newUrl` |
| `POST` | `/api/qb/v2/torrents/removeTrackers` | Form: `hash`, `urls` (pipe-separated) |
| `POST` | `/api/qb/v2/torrents/toggleSequentialDownload` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/addPeers` | Accepted |
| `POST` | `/api/qb/v2/torrents/increasePrio` | Accepted |
| `POST` | `/api/qb/v2/torrents/decreasePrio` | Accepted |
| `POST` | `/api/qb/v2/torrents/topPrio` | Accepted |
| `POST` | `/api/qb/v2/torrents/bottomPrio` | Accepted |
| `POST` | `/api/qb/v2/torrents/setAutoManagement` | Accepted |
| `POST` | `/api/qb/v2/torrents/setForceStart` | Accepted |
| `POST` | `/api/qb/v2/torrents/setSuperSeeding` | Accepted |
| `POST` | `/api/qb/v2/torrents/toggleFirstLastPiecePrio` | Accepted |

### Sync / Transfer

| Method | Path |
|--------|------|
| `GET` | `/api/qb/v2/sync/maindata` | Full (`rid=0`) and incremental (`rid>0`) torrent updates; includes current `categories` map and `tags` list |
| `GET` | `/api/qb/v2/transfer/info` |
| `GET` | `/api/qb/v2/transfer/speedLimitsMode` |
| `POST` | `/api/qb/v2/transfer/toggleSpeedLimitsMode` |
| `GET` | `/api/qb/v2/transfer/downloadLimit` |
| `POST` | `/api/qb/v2/transfer/setDownloadLimit` |
| `GET` | `/api/qb/v2/transfer/uploadLimit` |
| `POST` | `/api/qb/v2/transfer/setUploadLimit` |
| `POST` | `/api/qb/v2/transfer/banPeers` |

### Log / Search / RSS

These compatibility endpoints are present so qBittorrent clients can probe them safely. Search remains intentionally inert; RSS endpoints are backed by native RSS rules where available and degrade to compatible no-op shapes otherwise.

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/api/qb/v2/log/main` | Returns `[]` |
| `GET` | `/api/qb/v2/log/peers` | Returns `[]` |
| `GET` | `/api/qb/v2/search/status` | Returns stopped status |
| `GET` | `/api/qb/v2/search/categories` | Returns `[]` |
| `GET` | `/api/qb/v2/search/plugins` | Returns `[]` |
| `POST` | `/api/qb/v2/search/installPlugin` | Accepted |
| `POST` | `/api/qb/v2/search/uninstallPlugin` | Accepted |
| `POST` | `/api/qb/v2/search/enablePlugin` | Accepted |
| `POST` | `/api/qb/v2/search/updatePlugins` | Accepted |
| `POST` | `/api/qb/v2/search/start` | Returns `{ "id": 0 }` |
| `POST` | `/api/qb/v2/search/stop` | Accepted |
| `GET` | `/api/qb/v2/search/results` | Returns empty stopped result set |
| `POST` | `/api/qb/v2/search/delete` | Accepted |
| `GET` | `/api/qb/v2/rss/items` | Returns `{}` |
| `GET` | `/api/qb/v2/rss/rules` | Returns native RSS rules in qBit-shaped rule map |
| `GET` | `/api/qb/v2/rss/matchingArticles` | Returns names of native RSS rules matching `article` |
| `POST` | `/api/qb/v2/rss/setRule` | Creates/updates native RSS rule from qBit rule JSON |
| `POST` | `/api/qb/v2/rss/renameRule` | Renames native RSS rule |
| `POST` | `/api/qb/v2/rss/removeRule` | Deletes native RSS rule |
| `POST` | `/api/qb/v2/rss/addFolder`, `/addFeed`, `/removeItem`, `/moveItem`, `/markAsRead`, `/refreshItem` | Accepted as compatibility no-ops |

---

## Prometheus Metrics

Exposed at `GET /metrics` in Prometheus text format:

Native mode exposes engine/session/API metrics from `rusttorrentd`. Track 1
sidecar mode additionally exposes rTorrent sync-loop counters because it polls
rTorrent over XMLRPC.

| Metric | Type | Description |
|--------|------|-------------|
| `torrentng_torrents_total` | gauge | Total torrents in session |
| `torrentng_torrents_seeding` | gauge | Currently seeding |
| `torrentng_torrents_downloading` | gauge | Currently downloading |
| `torrentng_torrents_stopped` | gauge | Stopped |
| `torrentng_torrents_errored` | gauge | In error state |
| `torrentng_torrents_activity_{hot,warm,dormant}` | gauge | Activity-tier classification counts from the native tier policy |
| `torrentng_torrent_tasks_active` | gauge | Active per-torrent runtime tasks |
| `torrentng_fastresume_dirty_pieces` | gauge | Pieces validated since the last completed durability barrier |
| `torrentng_completed_piece_verify_from_{memory,disk}_total` | counter | Completed-piece verification source; memory verifies avoid read-after-write disk rereads |
| `torrentng_peers_connected` | gauge | Connected peers across all torrents |
| `torrentng_storage_file_pool_*` | gauge/counter | Native scheduler open-file cache capacity, open files, hits, misses, evictions, and idle closes |
| `torrentng_storage_*_queue_depth` | gauge | Native disk I/O and hashing queue depths |
| `torrentng_storage_{read,write}_ops_total` | counter | Positioned disk operations through native schedulers |
| `torrentng_storage_bytes_{read,written}_total` | counter | Bytes moved through native schedulers |
| `torrentng_storage_*_by_class_total{class=...}` | counter | Read/write operation and byte counters split by scheduler I/O class |
| `torrentng_storage_backend_selected{backend=...}` | gauge | Global storage backend selected at runtime (`pread` or future `uring`) |
| `torrentng_storage_backend_fixed_buffers_supported` | gauge | Whether the selected backend can use registered fixed buffers |
| `torrentng_storage_backend_read_*` | counter | Actual backend disk read operations and bytes, excluding peer-read cache hits |
| `torrentng_storage_*_latency_nanoseconds` | histogram/counter | Storage queue plus execution latency buckets and cumulative totals for read, write, sync, and hash work |
| `torrentng_storage_{sync,hash}_ops_total` | counter | Durability syncs and hashing-pool work |
| `torrentng_storage_preallocation_*_total` | counter | Preallocation failures and fallback events |
| `torrentng_storage_peer_read_cache_*` | gauge/counter | Peer-read readahead cache entries, hits, and misses |
| `torrentng_storage_peer_read_elevator_*` | gauge/counter | Peer-read elevator enablement, queue state, backend batches, and coalesced requests |
| `torrentng_storage_page_cache_advise_*` | counter | Page-cache advice calls (`SEQUENTIAL`, `WILLNEED`, `DONTNEED`) and failures from the storage scheduler |
| `torrentng_storage_sparse_*` | counter | Sparse recheck data extents, skipped hole bytes, and seek fallback count |
| `torrentng_piece_assembly_*` | gauge/counter | In-memory completed-piece assembly buffers, bytes, and budget evictions |
| `torrentng_memory_*` | gauge/counter | Resource governor cap, current process-owned usage, pressure state, per-class caps/usage, and denied allocations |
| `torrentng_api_requests_total` | counter | API requests served |
| `torrentng_sync_cycles_total` | counter | Track 1 sidecar rTorrent sync cycles completed |
| `torrentng_sync_errors_total` | counter | Track 1 sidecar rTorrent sync cycle errors |
