# TorrentNG API Reference

TorrentNG exposes the same client-facing API families in both backend
arrangements:

- **TorrentNG REST API** — `/api/v1/` — JSON REST, snake_case, designed for the
  WebUI and direct integrations. When `torrentngd` serves it directly, it is
  backed by durable TorrentNG client state; in a compatible-client deployment,
  `torrentng` projects the selected client's state through the same surface.
- **qBittorrent compat API** — `/api/v2/` and `/api/qb/v2/` — targeted as a drop-in replacement for the qBittorrent Web API v2; used by Prowlarr, Sonarr, Radarr, autobrr, cross-seed, etc.
- **Transmission RPC** — `/transmission/rpc` and `/api/transmission/rpc` — compatibility facade over the same torrent registry.
- **Deluge RPC** — Deluge-compatible facade for clients that expect Deluge method names, with parity tracked in the compatibility matrix.

All surfaces are served on the same port (default `8080`).

The API strategy is compatibility-first: existing tools can keep speaking the
client dialect they already support while TorrentNG projects those calls onto
one shared user-facing model. Endpoint availability does not by itself mean
full semantic parity; current TorrentNG-client, compatible-client, partial, and
gap status is tracked in
[CLIENT_COMPATIBILITY_MATRICES.md](CLIENT_COMPATIBILITY_MATRICES.md).

The machine-readable contract for the TorrentNG REST surface is
[API.openapi.json](API.openapi.json). It is intentionally scoped to the
implemented `/api/v1` surface; qBittorrent, Transmission, Deluge, and rTorrent
compatibility method matrices remain documented separately because their wire
contracts are client-specific.

For backend arrangement selection and TorrentNG-client versus compatible-client
behavior, see
[ENGINE_REWRITE.md](ENGINE_REWRITE.md).

---

## Authentication

When `auth.api_tokens` is configured, all non-public endpoints require one of:

```
Authorization: Bearer <token>
Cookie: tng_session=<token>
```

Public endpoints (never require auth): `/health`, the login/logout endpoints,
and the WebUI/static paths. `/metrics` is protected when API tokens are
configured; it is not a public exception.

## Request Correlation

Non-static API responses include an `X-Request-Id` header. If a client supplies a
bounded, printable `X-Request-Id`, TorrentNG preserves it in structured request
logs and echoes it back; otherwise the runtime generates a `tng-N` id. Health,
metrics, websocket, and static asset requests are intentionally excluded from
request logging noise. Request logs also include the socket peer address when
the runtime has connect-info available; forwarded headers are not trusted for
that field.

---

## Health And Capability Manifest

`GET /health` reports service readiness and a machine-readable capability
manifest. Existing readiness fields remain stable (`status`, `ready`,
`native_engine`, `torrent_count`), and the nested `engine.capabilities` object
advertises support for v1/v2/hybrid identity, `btih`/`btmh` magnets, durable
session/job state, storage safety, DHT/uTP policy, TorrentNG REST, qBittorrent,
Transmission, Deluge, migration, metrics, and diagnostics.
The uTP section distinguishes implementation capability from active runtime
paths through `utp_transport_paths`, which can include `outgoing_peer_wire`,
`metadata_fetch`, and `incoming_peer_wire` depending on the current
`TNG_UTP_*` environment policy.

The capability manifest uses `torrentng_rest` and `torrentng_sse` as the
canonical names for the first-party API and event stream. The older
`native_rest` and `native_sse` entries remain as compatibility aliases for
clients that persisted earlier manifests.

`/api/v1/engine` also exposes the selected backend adapter and its capabilities.
In a direct `torrentngd` deployment, `/health` reports `engine.mode` as
`torrentng` and `/api/v1/engine` reports `backend.type` as `torrentng`; in a
compatible-client deployment, the compatible-client service reports the
selected client type (`rtorrent`, `qbittorrent`, `transmission`, `deluge`, or
`torrentng`).
Adapter capability flags cover tags, categories, file priority,
tracker edit, recheck, torrent export, webseed reads, piece state/hash reads,
peer snapshots, explicit peer add/ban, queue ordering, per-torrent/global/share
limits, qBittorrent-style mode flags, location updates, torrent/file renames,
and runtime user-agent changes. The TorrentNG client supports the user-agent
mutation; configuration overlays and backend restart remain supervisor-owned or
unsupported.
Where
a flag is false, the relevant endpoint returns an explicit unsupported result
or the documented compatibility empty response instead of pretending mutation
success. A compatible-client facade may retain a projection-only setting for a
client, but that does not mean the selected client applies it.

The `engine.track1_sidecar_required` field is a retained compatibility field.
It is `false` when `torrentngd` serves the API directly; the field does not
mean the WebUI requires a third product or that the compatible-client service is
required for the TorrentNG client.

---

## TorrentNG REST API — `/api/v1`

### Auth

| Method | Path | Description |
|---|---|---|
| `POST` | `/api/v1/auth/login` | WebUI session login. With `auth.api_tokens`, username or password must match an API token; success returns `Ok.` and a `tng_session` cookie. |
| `POST` | `/api/v1/auth/logout` | WebUI logout probe; expires the `tng_session` cookie. |

### Torrents

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/api/v1/torrents` | List torrents with filter/sort/page |
| `GET`  | `/api/v1/torrents/live?hashes=...` | Read bounded live rates and bytes-left for explicitly requested visible torrents |
| `POST` | `/api/v1/torrents` | Add torrent as JSON (`torrent_b64` or `magnet`, `save_path`, optional `category`, `tags`, `start`) |
| `GET`  | `/api/v1/torrents/:hash` | Get single torrent by hash |
| `PUT`  | `/api/v1/torrents/:hash` | Update torrent metadata (`{ name, save_path }`; at least one required) |
| `DELETE` | `/api/v1/torrents/:hash` | Remove torrent (`?delete_files=true` queues an asynchronous payload delete and returns `{ job_id, state }`; otherwise `204`) |
| `POST` | `/api/v1/torrents/:hash/start` | Start torrent |
| `POST` | `/api/v1/torrents/:hash/resume` | Resume torrent (alias of `start`) |
| `POST` | `/api/v1/torrents/:hash/stop` | Stop torrent |
| `POST` | `/api/v1/torrents/:hash/pause` | Pause torrent (alias of `stop`) |
| `POST` | `/api/v1/torrents/:hash/recheck` | Force hash check |
| `POST` | `/api/v1/torrents/:hash/reannounce` | Force tracker announce |
| `GET`  | `/api/v1/torrents/:hash/trackers` | List trackers |
| `PATCH` | `/api/v1/torrents/:hash/trackers` | Add/remove/edit trackers (`{ add: ["url"], remove: ["url"], edit: [{ orig_url, new_url }] }`); the TorrentNG client applies this by replacing its tracker list after the patch |
| `GET`  | `/api/v1/torrents/:hash/files` | List files |
| `PATCH` | `/api/v1/torrents/:hash/files` | Set file priorities and/or paths (`{ files: [{index, priority, path}] }`); the TorrentNG client routes each file priority and rename update through its engine |
| `PUT`  | `/api/v1/torrents/:hash/category` | Set or clear category (`{ category: "name" }`, or `null`/empty to clear) |
| `GET`  | `/api/v1/torrents/:hash/limits` | Read per-torrent limits and mode flags |
| `PUT`  | `/api/v1/torrents/:hash/limits` | Merge per-torrent limits (`download_limit`, `upload_limit`, `max_connections`, `seed_ratio_limit`, `seed_idle_limit`, `sequential_download`, `first_last_piece_prio`, `force_start`, `super_seeding`, `auto_tmm`, `auto_management`; use `null` for nullable limits) |
| `POST` | `/api/v1/torrents/:hash/peers` | Add explicit peers (`{ peers: ["host:port"] }`) |
| `POST` | `/api/v1/torrents/queue` | Move torrents in queue order (`{ hashes: ["..."], move: "up" \| "down" \| "top" \| "bottom" }`) |
| `PATCH` | `/api/v1/torrents/:hash/tags` | Add/remove tags (`{ add: ["a"], remove: ["b"] }`); the TorrentNG client routes tag changes through its engine label update path |
| `GET` | `/api/v1/events` | Server-sent torrent delta stream; accepts `last_known_revision` for incremental reconnects |
| `GET` | `/api/v1/jobs` | List active durable engine jobs with progress, checkpoint, and last-error fields |
| `GET` | `/api/v1/session-events` | Recent durable TorrentNG session events; query: `limit`, `torrent`, `kind`, `level`, `last_known_id` |
| `GET` | `/api/v1/logs` | Recent durable TorrentNG app/backend events plus rTorrent log-ingest events when that adapter is selected; query: `limit`, `kind`, `level`, `last_known_id` |
| `GET` | `/api/v1/transfer/info` | Aggregate transfer rates, byte counters, DHT node count, and global limit state |
| `GET` | `/api/v1/transfer/limits` | Read global transfer limits and speed-limits mode |
| `PUT` | `/api/v1/transfer/limits` | Merge global transfer limits (`download_limit`, `upload_limit`, `speed_limits_mode`) |
| `GET` | `/api/v1/session/features` | Read runtime network feature switches (`dht`, `pex`) |
| `PUT` | `/api/v1/session/features` | Merge runtime network feature switches; DHT starts/stops at runtime and PEX affects future peer extension handshakes |

`/api/v1/logs` returns retained operator events newest-first. Important `kind` values include `sidecar_started`, `rtorrent_log`, `rtorrent_log_ingest_error`, `rtorrent_log_ingest_recovered`, `rtorrent_sync_error`, `rtorrent_sync_recovered`, `rtorrent_stats_error`, `rtorrent_stats_recovered`, `rtorrent_user_agent_error`, `rtorrent_peer_id_error`, `torrent_added`, `torrent_removed`, `torrent_updated`, `categories_updated`, `tags_updated`, `saved_views_updated`, `ratio_groups_updated`, `rss_rules_updated`, `workflows_updated`, `workflow_runs_updated`, `settings_changed`, and `admin_restart_requested`. The `rtorrent_sync_*` and `rtorrent_stats_*` names are retained as legacy event kinds, but their payloads include the selected backend. Payloads are sanitized: magnet URLs, auth material, full filesystem paths, and raw user-agent strings are not stored.

#### `GET /api/v1/torrents` query parameters

| Parameter | Type | Description |
|-----------|------|-------------|
| `filter` | string | Name or info-hash substring match (case-insensitive) |
| `status` | string | `seeding` \| `downloading` \| `stopped` \| `checking` \| `error` \| `tracker_error` plus derived `active`, `inactive`, `running`, `completed`, `queued`, and `paused` buckets |
| `category` | string | Exact category name |
| `tag` | string | Exact tag name |
| `tracker` | string | Case-insensitive substring match against persisted tracker URLs; requires a backend that exposes tracker rows |
| `media_type` | string | Inferred type: `ebook` \| `tv` \| `video` \| `audio` \| `image` \| `game` \| `software` \| `other` |
| `sort` | string | `name` \| `size` \| `added` \| `ratio` \| `speed_down` \| `speed_up` \| `progress` |
| `dir` | string | `asc` \| `desc` |
| `limit` | int | Max rows (1–5000, default 200) |
| `offset` | int | Page offset within the immutable snapshot (default 0) |
| `snapshot` | uint | Snapshot generation returned by a previous page; pin this on every subsequent page |

Response: `{ snapshot: uint, total: int, torrents: TorrentRow[] }`. Each
TorrentNG-client summary includes `downloaded` as cumulative transfer accounting and
`amount_left` as the live payload bytes remaining; use `amount_left` for
completion/progress after rechecks. The server
materializes a bounded immutable summary snapshot and lazily caches sort order;
the page itself is bounded to `limit`. If a requested `snapshot` has expired
from the server cache, the endpoint returns `410 Gone` and the client must
restart pagination without that cursor. Without a cursor, a recent snapshot may
be reused for up to 750 ms to prevent concurrent list callers from repeatedly
scanning live actor state.

This snapshot contract applies when `torrentngd` serves the route. In a
compatible-client deployment the same route reads the service's local SQLite
projection and returns the bounded `{ total, torrents }` envelope without a
TorrentNG-client snapshot token; it is eventually consistent with the selected
client. Compatible-client callers must not use `offset` pagination as if it
were an immutable multi-request snapshot.

#### `GET /api/v1/torrents/live`

Pass a comma-separated `hashes` query parameter containing at most 128
40/64-character hexadecimal info hashes. The WebUI sends only incomplete,
active rows intersecting the current viewport. The response is
`{ sampled_at, torrents: [{ hash, amount_left, download_rate, upload_rate, sampled_at }] }`.
The TorrentNG client queries only the requested promoted torrent tasks with
bounded concurrency; the compatible-client service reads the cached SQLite
projection in one query and does not contact the selected client.

`GET /api/v1/events` emits `torrent_delta` events with a `cursor` field equal to
the registry revision. The initial snapshot is emitted in bounded chunks
(default 500 summaries, maximum 1,000) at one revision. Initial events set
`snapshot: true`; the final chunk also sets `snapshot_complete: true`. Later
events contain only mutated or removed torrents from the bounded mutation
journal. A reconnect with `last_known_revision` resumes from that cursor. If
the journal has expired, the next event is a one-time bounded snapshot resync.

After the first generation, list snapshot refreshes use the mutation journal
when it still covers the cached revision; the server falls back to a full
registry projection only when the journal or base snapshot is unavailable.

`PUT /api/v1/torrents/:hash` returns `202 Accepted` with `{ job_id, state }`
when changing `save_path` requires a filesystem move. The move is executed by
the bounded storage worker pool; inspect `/api/v1/jobs` using the returned id.
Name-only updates and already-local metadata changes retain `204 No Content`.

TorrentNG REST list, delta-stream, and torrent-detail snapshots are charged to the
`api_snapshot` memory class. When that budget is exhausted the API returns
`503` with `api snapshot memory budget exhausted` instead of cloning an
unbounded response.

The qBittorrent facade applies the same budget to `/api/qb/v2/torrents/info`
and `/api/qb/v2/sync/maindata`. Bounded pages charge for their selected output;
full compatibility responses still charge for and materialize the requested
output. Snapshot caching avoids repeated registry clones, and `sync/maindata`
uses the registry mutation journal for retained revisions.

The compatible-client qBittorrent `sync/maindata` response is capped at
10,000 changed torrents because that upstream method has no page or snapshot
parameter. It returns `413 Payload Too Large` rather than claiming a complete
but truncated full or incremental update; use the TorrentNG REST paged API for
larger collections.

The Deluge facade applies the same budget to `web.update_ui`,
`core.get_torrents_status`, and `core.get_torrent_status`.
The Transmission facade applies it to `torrent-get`, scaled by torrent count
and requested field count. The rTorrent XMLRPC facade applies it to
`d.multicall`/`d.multicall2`, scaled by torrent count and requested command
count. Deluge/Transmission full-list compatibility calls and rTorrent
`d.multicall` reject responses over 10,000 torrents because those upstream
contracts do not provide a range or snapshot cursor; use the TorrentNG REST
paged API for large collections.

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
- `state`: 0=idle, 1=active, 2=checking, 3=error, 4=metadata pending, 5=queued (TorrentNG-client adapter extensions)

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
| `POST` | `/api/v1/storage/plan` | Preview move/import/delete storage plans (`{ operation, source, destination, target, bytes, available_bytes, hardlink_or_copy, roots, affected_torrents }`); `bytes` is required for move/import so copy and rename verification cannot silently use zero, while delete bytes remain optional. Import uses staged copy unless `hardlink_or_copy` allows same-filesystem hardlinks |
| `POST` | `/api/v1/storage/execute` | Execute a root-confined move/import/delete storage plan through durable engine storage-plan jobs; execution uses server-configured storage roots, ignores client-supplied `roots`, and rejects non-empty `completed_steps` because checkpoint indexes are server-owned (move/delete require non-empty `affected_torrents` containing existing torrent hashes so live payload owners can be quiesced; import may omit it). Resume is driven by the durable job checkpoint and live filesystem reconciliation. A save-path move is reported as `commit_pending` after filesystem completion until the engine publishes the new path to the durable torrent row; it is not terminal and is recovered after a crash. If reconciliation, rollback, or destructive-step verification cannot prove a safe filesystem state, the job fails and affected torrents remain quiesced for manual recovery. |
| `GET` | `/api/v1/tracker-health` | Aggregate durable tracker rows by tracker URL with torrent/active/complete/error/peer counts; `peer_count` is the tracker-reported peer/leecher count, not the number of currently connected local sessions |
| `GET` | `/api/v1/engine` | Runtime backend type/capabilities plus rTorrent provenance, XMLRPC probes, tracker-stack telemetry, and profile drift when the selected backend is rTorrent |
| `GET` | `/api/v1/engine/commands` | Full XMLRPC command index when the rTorrent integration is selected; direct `torrentngd` service returns explicit `501 NOT_IMPLEMENTED` |
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
| `POST` | `/api/v1/workflows/:id` | Run workflow rule (`{ dry_run }`); TorrentNG-client category/location actions execute. TorrentNG-client webhook and script actions are recorded as unsupported; a compatible-client integration may execute them only when its own backend/configuration allows it |
| `DELETE` | `/api/v1/workflows/:id` | Delete workflow rule |
| `GET` | `/api/v1/workflow-runs` | List the most recent workflow run audit records, capped to the latest 200 |

### RSS Rules

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/rss-rules` | List RSS automation rules |
| `POST` | `/api/v1/rss-rules` | Create/update RSS rule (`{ id, name, enabled, feed_url, include, exclude, category, save_path, tags, start }`) |
| `POST` | `/api/v1/rss-rules/test` | Test a sample `{ title, link }` against configured rules |
| `POST` | `/api/v1/rss-rules/apply` | Preview or apply a sample `{ title, link, dry_run }`; TorrentNG-client runs accept magnet links through the client. HTTP torrent downloads are implemented only by the compatible-client service |
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
| `GET`  | `/api/v1/settings/user-agent` | Get current runtime user-agent string when supported by the selected backend |
| `PUT`  | `/api/v1/settings/user-agent` | Set user-agent (`{ user_agent: "..." }`) when supported; the TorrentNG client persists and applies it through its engine, while rTorrent behavior depends on its packaged build |

### Infrastructure

| Method | Path | Description |
|--------|------|-------------|
| `GET`  | `/health` | Health check. The compatible-client service returns selected-backend reachability in `backend`, preserves legacy `rtorrent`, and includes cache count. Supported integrations are rTorrent, qBittorrent, Transmission, Deluge, and a separate TorrentNG client. |
| `GET`  | `/metrics` | Prometheus text format metrics |
| `GET`  | `/ws` | WebSocket — upgrade to receive live events |

When configured with `backend.type = "torrentng"`, the compatible-client service
forwards torrent add/remove, pause/resume, recheck/reannounce, category/tag
changes, location/name updates, file-priority and file-rename changes, and
tracker patch operations to a separate `torrentngd` TorrentNG client over these
`/api/v1` endpoints.

#### WebSocket events

```json
{ "type": "torrent_updated", "hash": "abc123" }
{ "type": "torrent_removed", "hash": "abc123" }
{ "type": "stats", "upload_speed": 1048576, "download_speed": 524288 }
```

If a client falls behind the bounded event buffer, the compatible-client
service emits a
resync_required event with reason event_stream_lagged and the number of
dropped events. The client must refresh its bounded torrent list; event
delivery is not a durable replay protocol. A socket that cannot accept a
message for five seconds is closed.

---

## qBittorrent Compat API — `/api/v2` or `/api/qb/v2`

Implements the qBittorrent Web API v2. By default it advertises qBittorrent `5.0.0` / Web API `2.11.0`; lab builds can override those API-facing values with the `TNG_QBITTORRENT_*` identity environment variables. Configure external tools to point at this server as if it were qBittorrent.

### Auth

| Method | Path | Notes |
|--------|------|-------|
| `POST` | `/api/qb/v2/auth/login` | qBittorrent-compatible login probe; accepts credentials and returns `Ok.` with a `SID` cookie for clients that require qBit session shape |
| `POST` | `/api/qb/v2/auth/logout` | Expires the qBittorrent-compatible `SID` cookie |

### App

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/api/qb/v2/app/version` |
| `GET` | `/api/qb/v2/app/webapiVersion` |
| `GET` | `/api/qb/v2/app/buildInfo` |
| `GET` | `/api/qb/v2/app/preferences` | Includes queue defaults plus backend-derived `dht`/`pex` status when known and `network_http_user_agent` when supported; TorrentNG-client preferences, cookies, and API-key state are durable |
| `GET` | `/api/qb/v2/app/defaultSavePath` |
| `POST` | `/api/qb/v2/app/setPreferences` | Form: `json` preference object; TorrentNG-client mode applies `dht`, `pex`, and the supported user-agent setting; other accepted keys are compatibility-only facade overrides and are not runtime enforcement unless the capability manifest says so |
| `GET` | `/api/qb/v2/app/getCookies` | Returns durable facade-stored cookie objects sorted by host/name when a TorrentNG client is attached; no-client instances are process-local |
| `POST` | `/api/qb/v2/app/setCookies` | Accepts JSON array, `{ "cookies": [...] }`, or form `cookies=<json array>`; bounded and durable when the engine is attached |
| `POST` | `/api/qb/v2/app/rotateAPIKey` | Stores and returns a generated facade API key as `{ "apiKey": "..." }`; this is compatibility state, not an authentication credential |
| `POST` | `/api/qb/v2/app/deleteAPIKey` | Clears the facade API key |

### Torrents

| Method | Path | Notes |
|--------|------|-------|
| `GET`  | `/api/qb/v2/torrents/info` | Filter params: `filter`, `category`, `tag`, `sort`, `reverse`, `limit`, `offset`, optional `snapshot`; default `limit` is 200 and the TorrentNG facade clamps it to 5,000; returns `X-TorrentNG-Snapshot` for stable offset pagination |
| `GET`  | `/api/qb/v2/torrents/properties` | Query: `hash`; returns cached torrent properties |
| `POST` | `/api/qb/v2/torrents/add` | Multipart: `urls`, `savepath`, `category`, `tags`, `paused`, `stopped`, `skip_checking`, `autoTMM`, `contentLayout`, `ratioLimit`, `seedingTimeLimit`, `torrents` (file) |
| `POST` | `/api/qb/v2/torrents/pause` / `/stop` | Form: `hashes` (pipe-separated or `all`) |
| `POST` | `/api/qb/v2/torrents/resume` / `/start` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/delete` | Form: `hashes`, `deleteFiles` |
| `POST` | `/api/qb/v2/torrents/recheck` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/reannounce` | Form: `hashes` |
| `GET`  | `/api/qb/v2/torrents/trackers` | Query: `hash` |
| `GET`  | `/api/qb/v2/torrents/webseeds` | Query: `hash`; backend-backed where supported, otherwise `[]` |
| `GET`  | `/api/qb/v2/torrents/files` | Query: `hash` |
| `GET`  | `/api/qb/v2/torrents/pieceStates` | Query: `hash`; backend-backed where supported, otherwise `[]` |
| `GET`  | `/api/qb/v2/torrents/pieceHashes` | Query: `hash`; backend-backed where supported, otherwise `[]` |
| `GET`  | `/api/qb/v2/torrents/export` | Query: `hash`; streams `application/x-bittorrent` where the backend has the original torrent blob |
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
| `GET`  | `/api/qb/v2/torrents/downloadLimit` | Query: `hashes`; returns `{ "hash": limit }` where supported |
| `POST` | `/api/qb/v2/torrents/setDownloadLimit` | Form: `hashes`, `limit`; backend-backed where supported |
| `GET`  | `/api/qb/v2/torrents/uploadLimit` | Query: `hashes`; returns `{ "hash": limit }` where supported |
| `POST` | `/api/qb/v2/torrents/setUploadLimit` | Form: `hashes`, `limit`; backend-backed where supported |
| `POST` | `/api/qb/v2/torrents/setShareLimits` | Form: `hashes`, `ratioLimit`, `seedingTimeLimit` |
| `POST` | `/api/qb/v2/torrents/setLocation` | Form: `hashes`, `location` |
| `POST` | `/api/qb/v2/torrents/setSavePath` | Form: `hashes`, `location` |
| `POST` | `/api/qb/v2/torrents/addTrackers` | Form: `hashes`, `urls` (newline-separated) |
| `POST` | `/api/qb/v2/torrents/setAutoTMM` | Backend-backed where supported |
| `POST` | `/api/qb/v2/torrents/editTracker` | Form: `hash`, `origUrl`, `newUrl` |
| `POST` | `/api/qb/v2/torrents/removeTrackers` | Form: `hash`, `urls` (pipe-separated) |
| `POST` | `/api/qb/v2/torrents/toggleSequentialDownload` | Form: `hashes` |
| `POST` | `/api/qb/v2/torrents/addPeers` | Engine-backed where supported |
| `POST` | `/api/qb/v2/torrents/increasePrio` | Engine-backed where supported |
| `POST` | `/api/qb/v2/torrents/decreasePrio` | Engine-backed where supported |
| `POST` | `/api/qb/v2/torrents/topPrio` | Engine-backed where supported |
| `POST` | `/api/qb/v2/torrents/bottomPrio` | Engine-backed where supported |
| `POST` | `/api/qb/v2/torrents/setAutoManagement` | Backend-backed where supported |
| `POST` | `/api/qb/v2/torrents/setForceStart` | Backend-backed where supported |
| `POST` | `/api/qb/v2/torrents/setSuperSeeding` | Backend-backed where supported |
| `POST` | `/api/qb/v2/torrents/toggleFirstLastPiecePrio` | Backend-backed where supported |

For a response containing more than 200 torrents, the direct TorrentNG qBittorrent
facade does not issue per-torrent actor queries for transient tracker, swarm,
queue, and limit fields. Those fields use compatibility defaults for that
large projection; durable torrent fields and aggregate transfer statistics
remain available. This cutoff prevents a large request from becoming one
sequential engine-actor round trip per torrent. Small interactive pages retain
the live projections. Explicitly requesting a large page still serializes that
page, so callers should use bounded pages and a pinned snapshot rather than
treating this endpoint as an unbounded export.

`POST /api/qb/v2/app/shutdown` requests graceful daemon shutdown when the
TorrentNG client daemon wires the facade's shutdown notifier. `sendTestEmail` is exposed
for method compatibility but returns `501 Not Implemented`.

In the TorrentNG-client arrangement, qBittorrent mode mutations that have no equivalent
runtime behavior (`setAutoTMM`, `setAutoManagement`, and `setForceStart`) return
`501 Not Implemented`. Queue order, sequential/first-last selection,
super-seeding, limits, categories, tags, and bans use the engine paths
documented by the capability manifest.

qBittorrent tag mutators reject empty or dropped comma-separated segments;
`setTags` may use an empty value to clear all tags. Legacy compatibility
mutators do not silently discard malformed values.

### Sync / Transfer

| Method | Path |
|--------|------|
| `GET` | `/api/qb/v2/sync/maindata` | Full (`rid=0`) and registry-revision incremental (`rid>0`) torrent updates; retained revisions return only changed torrents and removals, while TorrentNG-client stale revisions fall back to a full update; includes current `categories` map and `tags` list. In compatible-client integration, `rid` is a durable SQLite revision with bounded deletion tombstones; stale or over-limit deltas return `413`, and full responses over 10,000 torrents also return `413` because qBit provides no page/cursor contract. |
| `GET` | `/api/qb/v2/transfer/info` | Returns aggregate rates plus current global speed-limit state |
| `GET` | `/api/qb/v2/transfer/speedLimitsMode` | Returns `1` when alternate/global speed-limit mode is enabled, otherwise `0` |
| `POST` | `/api/qb/v2/transfer/toggleSpeedLimitsMode` | Toggles backend global speed-limit mode where supported |
| `GET` | `/api/qb/v2/transfer/downloadLimit` | Returns current global download limit |
| `POST` | `/api/qb/v2/transfer/setDownloadLimit` | Form: `limit`; backend-backed where supported |
| `GET` | `/api/qb/v2/transfer/uploadLimit` | Returns current global upload limit |
| `POST` | `/api/qb/v2/transfer/setUploadLimit` | Form: `limit`; backend-backed where supported |
| `POST` | `/api/qb/v2/transfer/banPeers` | Backend-backed where supported; the TorrentNG qBittorrent facade also persists banned peers into `app/preferences.banned_ips` |

### Compatibility failure semantics

The Deluge and Transmission facades require an attached TorrentNG client for
client-backed mutations. Empty-target lifecycle requests remain successful for
client compatibility; non-empty requests without an engine return an explicit
error. Deluge path-based torrent loads, plugin/configuration writes, and
notification writes are rejected. Transmission `session-close` requests
graceful daemon shutdown when the TorrentNG client wires its supervisor notifier;
port testing is unavailable;
its free-space response probes healthy configured TorrentNG storage roots. The
rTorrent library boundary accepts embedded raw metainfo and magnets but
rejects unsafe path-based loads. These boundaries prevent a facade response
from implying that a torrent mutation or filesystem operation was applied when
it was not.

### Pure-v2 boundary

Pure-v2 torrents are an explicit partial-support boundary, not a hidden
capability claim. TorrentNG-client parsing, storage projection, file-root recheck, and
metadata placeholders are supported. Pure-v2 metadata completion, peer
transfer, and tracker lifecycle are not implemented and return explicit
unsupported errors (HTTP maps these to `501 Not Implemented` where the
operation is exposed). The TorrentNG-client capability manifest reports completion and
transfer as unsupported. This remains intentional until there is a complete
v2 piece-transfer, peer-wire, and tracker design with independent evidence.

The rTorrent facade follows the same boundary and is a library-only entry
point; see [RTORRENT_LIBRARY_API.md](RTORRENT_LIBRARY_API.md).

### Log / Search / RSS

These compatibility endpoints are present so qBittorrent clients can probe them safely. Search keeps plugin and job lifecycle state while returning inert local result sets. RSS folder, feed, and rule state is durable through the engine settings store when a TorrentNG client is attached; no-client test/facade instances use process-local state. RSS automation is limited to the documented TorrentNG-client magnet path; unsupported external actions fail explicitly.

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/api/qb/v2/log/main` | Returns retained app/session/rTorrent log events in qBittorrent log shape; supports `limit`, `last_known_id`, `normal`, `info`, `warning`, `critical` |
| `GET` | `/api/qb/v2/log/peers` | Returns backend peer snapshots in qBittorrent peer log shape where supported; otherwise `[]` |
| `GET` | `/api/qb/v2/search/status` | Returns stopped/running status plus known plugin state |
| `GET` | `/api/qb/v2/search/categories` | Returns enabled compatibility plugin categories |
| `GET` | `/api/qb/v2/search/plugins` | Returns installed compatibility plugin records |
| `POST` | `/api/qb/v2/search/installPlugin` | Records plugin source/name metadata |
| `POST` | `/api/qb/v2/search/uninstallPlugin` | Removes plugin records |
| `POST` | `/api/qb/v2/search/enablePlugin` | Updates plugin enabled state |
| `POST` | `/api/qb/v2/search/updatePlugins` | Safe compatibility refresh probe; local plugin records are unchanged |
| `POST` | `/api/qb/v2/search/start` | Returns a stable per-process search job id |
| `POST` | `/api/qb/v2/search/stop` | Stops the requested compatibility search job |
| `GET` | `/api/qb/v2/search/results` | Returns the requested job plus empty local result set |
| `POST` | `/api/qb/v2/search/delete` | Deletes the requested compatibility search job |
| `GET` | `/api/qb/v2/rss/items` | Returns compatibility folder/feed state |
| `GET` | `/api/qb/v2/rss/rules` | Returns compatibility qBit-shaped rule map |
| `GET` | `/api/qb/v2/rss/matchingArticles` | Returns known rule names |
| `POST` | `/api/qb/v2/rss/setRule` | Creates/updates bounded RSS rule JSON; state is durable with the TorrentNG client attached |
| `POST` | `/api/qb/v2/rss/renameRule` | Renames compatibility RSS rule |
| `POST` | `/api/qb/v2/rss/removeRule` | Deletes compatibility RSS rule |
| `POST` | `/api/qb/v2/rss/addFolder`, `/addFeed`, `/removeItem`, `/moveItem`, `/markAsRead`, `/refreshItem` | Maintains bounded qBit-shaped folder/feed item state, durable with the TorrentNG client attached and process-local otherwise |

---

## Prometheus Metrics

Exposed at `GET /metrics` in Prometheus text format:

The TorrentNG client exposes engine/session/API metrics directly from
`torrentngd`. The compatible-client service additionally exposes backend
sync-loop counters because it polls the selected client adapter.

| Metric | Type | Description |
|--------|------|-------------|
| `torrentng_torrents_total` | gauge | Total torrents in session |
| `torrentng_torrents_seeding` | gauge | Currently seeding |
| `torrentng_torrents_downloading` | gauge | Currently downloading |
| `torrentng_torrents_stopped` | gauge | Stopped |
| `torrentng_torrents_errored` | gauge | In error state |
| `torrentng_torrents_activity_{hot,warm,dormant}` | gauge | Activity-tier classification counts from the TorrentNG-client tier policy |
| `torrentng_dormant_runtime_heap_bytes` | gauge | Heap retained by compact dormant runtime projections; this does not certify total process RSS |
| `torrentng_torrent_tasks_active` | gauge | Active per-torrent runtime tasks |
| `torrentng_fastresume_dirty_pieces` | gauge | Pieces validated since the last completed durability barrier |
| `torrentng_completed_piece_verify_from_{memory,disk}_total` | counter | Completed-piece verification source; memory verifies avoid read-after-write disk rereads |
| `torrentng_{download,upload}_rate_bytes_per_second` | gauge | Aggregate current peer transfer rates; served from the cached TorrentNG-client stats snapshot |
| `torrentng_peers_connected` | gauge | Connected peers across all torrents |
| `torrentng_storage_jobs_{inflight,queue_depth,capacity}` | gauge | Retained requests, pending dispatcher-channel requests, and end-to-end capacity for durable storage-plan background work |
| `torrentng_storage_workers_healthy` | gauge | Storage-job supervisor health (`1` healthy, `0` unhealthy); an unhealthy value means new durable storage work cannot be trusted |
| `torrentng_storage_workers` | gauge | Configured blocking storage worker count |
| `torrentng_api_snapshot_refreshes_total` | counter | Immutable API snapshot generations built across TorrentNG-client and qBittorrent facades |
| `torrentng_api_snapshot_incremental_updates_total` | counter | Snapshot generations advanced from retained registry mutation-journal changes |
| `torrentng_api_snapshot_expired_total` | counter | Pagination cursors rejected because their immutable snapshot expired |
| `torrentng_api_sse_{resyncs,events,lagged,disconnects}_total` | counter | SSE journal-expiry resyncs, emitted torrent-delta events, delayed stream polls, and dropped stream instances |
| `torrentng_api_sse_clients` | gauge | Active TorrentNG SSE clients |
| `torrentng_api_response_bytes_estimated_total` | counter | Estimated bytes emitted by bounded list and SSE responses; an estimate, not wire accounting |
| `torrentng_dht_*` | gauge | TorrentNG-client DHT routing table, lookup, tracked torrent, and announced peer cache counts |
| `torrentng_storage_file_pool_*` | gauge/counter | TorrentNG-client scheduler open-file cache capacity, open files, metadata bytes, hits, misses, evictions, and idle closes |
| `torrentng_storage_*_queue_depth` | gauge | TorrentNG-client disk I/O and hashing queue depths |
| `torrentng_storage_device_queue_{capacity,available}` | gauge | Process-level per-device storage queue permits across running torrent schedulers |
| `torrentng_storage_queued_disk_bytes` | gauge | Process-owned payload bytes currently reserved by queued or active disk, hash, and peer-read elevator jobs |
| `torrentng_storage_queue_full_total` | counter | Disk or hash jobs denied because the bounded per-mount storage queue was full |
| `torrentng_storage_{read,write}_ops_total` | counter | Positioned disk operations through TorrentNG-client schedulers |
| `torrentng_storage_bytes_{read,written}_total` | counter | Bytes moved through TorrentNG-client schedulers |
| `torrentng_storage_*_by_class_total{class=...}` | counter | Read/write operation and byte counters split by scheduler I/O class |
| `torrentng_storage_backend_selected{backend=...}` | gauge | Global storage backend selected at runtime (`pread` or explicit `uring`) |
| `torrentng_storage_backend_fixed_buffers_supported` | gauge | Whether the selected backend can use registered fixed buffers |
| `torrentng_storage_backend_registered_files_supported` | gauge | Whether the selected backend can use registered file slots |
| `torrentng_storage_backend_{max_batch_len,fixed_buffer_bytes}` | gauge | Selected backend batching and fixed-buffer sizing |
| `torrentng_storage_backend_fixed_buffer_strategy{strategy=...}` | gauge | Active fixed-buffer strategy (`disabled`, reserved `worker_copy`, or `frame_pool_slots`) |
| `torrentng_storage_backend_fixed_buffer_worker_copy` | gauge | Whether fixed-buffer submissions copy through backend-private worker buffers |
| `torrentng_storage_backend_frame_pool_slots_supported` | gauge | Whether fixed-buffer submissions use registered storage frame-pool slots directly |
| `torrentng_storage_backend_read_*` | counter | Actual backend disk read operations and bytes, excluding peer-read cache hits |
| `torrentng_storage_*_latency_nanoseconds` | histogram/counter | Storage queue plus execution latency buckets and cumulative totals for read, write, sync, and hash work |
| `torrentng_storage_*_latency_nanoseconds_by_device{device=...,profile=...}` | histogram/counter | Storage read/write/sync/hash latency histograms and totals split by resolved storage device/profile |
| `torrentng_storage_{sync,hash}_ops_total` | counter | Durability syncs and hashing-pool work |
| `torrentng_storage_preallocation_*_total` | counter | Preallocation failures and fallback events |
| `torrentng_storage_peer_read_cache_*` | gauge/counter | Peer-read readahead cache entries, hits, and misses |
| `torrentng_storage_peer_read_elevator_*` | gauge/counter | Peer-read elevator enablement, queue state, queue-full backpressure, backend batches, and coalesced requests |
| `torrentng_storage_page_cache_advise_*` | counter | Page-cache advice calls (`SEQUENTIAL`, `WILLNEED`, `DONTNEED`) and failures from the storage scheduler |
| `torrentng_storage_sparse_*` | counter | Sparse recheck data extents, skipped hole bytes, and seek fallback count |
| `torrentng_piece_assembly_*` | gauge/counter | In-memory completed-piece assembly buffers, bytes, and budget evictions |
| `torrentng_peer_request_window_reductions_total` | counter | Peer request refills reduced because memory pressure limited in-flight piece data |
| `torrentng_peer_{rx,tx}_buffer_bytes` | gauge | Process-owned peer buffer pressure from outstanding receive requests and upload buffers; upload block assembly is also charged to the `peer_buffer` memory class while in flight |
| `torrentng_peer_command_queue_*` | gauge/counter | Active peer command queue capacity, depth, and full-send backpressure events |
| `torrentng_tracker_peer_cache_*` | gauge/counter | Tracker peer addresses retained or dropped by bounded per-torrent peer caches |
| `torrentng_hot_torrent_memory_estimated_bytes{rank,info_hash}` | gauge | Top active torrents by estimated process-owned memory, capped to the largest ten torrents |
| `torrentng_hot_torrent_piece_assembly_bytes{rank,info_hash}` | gauge | Piece-assembly portion of each hot-torrent memory estimate |
| `torrentng_hot_torrent_peer_buffer_bytes{rank,info_hash}` | gauge | Peer rx/tx buffer portion of each hot-torrent memory estimate |
| `torrentng_hot_torrent_tracker_peer_bytes{rank,info_hash}` | gauge | Tracker peer-cache portion of each hot-torrent memory estimate |
| `torrentng_hot_torrent_peer_command_queue_bytes{rank,info_hash}` | gauge | Peer command queue portion of each hot-torrent memory estimate |
| `torrentng_hot_torrent_storage_cache_bytes{rank,info_hash}` | gauge | Per-torrent storage cache portion of each hot-torrent memory estimate |
| `torrentng_memory_*` | gauge/counter | Resource governor cap, current process-owned usage, pressure state, per-class caps/usage, and denied allocations, including the `queued_disk` class |
| `torrentng_api_requests_total` | counter | API requests served |
| `torrentng_sync_cycles_total` | counter | Compatible-client backend sync cycles completed |
| `torrentng_sync_errors_total` | counter | Compatible-client backend sync cycle errors |
