# API Reference

All responses are JSON with snake_case keys. All timestamps are Unix epoch seconds (int64).

Base URL: `http://localhost:8080` (configurable)

## Authentication

Two auth modes:

**Session token (browser):** `POST /api/v1/auth/login` → set-cookie `rtng_session`

**API token (automation):** `Authorization: Bearer <token>` header

CSRF protection: state-changing requests from browser require `X-RTNG-CSRF: 1` header (set automatically by the WebUI).

---

## Native API — /api/v1/

### Auth

```
POST /api/v1/auth/login
  body: { "username": str, "password": str }
  → 200 { "token": str }  (also sets session cookie)

POST /api/v1/auth/logout
  → 200

GET  /api/v1/auth/me
  → 200 { "username": str, "permissions": [...] }
```

### Torrents

```
GET /api/v1/torrents
  query:
    filter=<string>         full-text search
    status=downloading|seeding|stopped|checking|error
    category=<name>
    tag=<name>
    tracker=<domain>
    sort=name|size|added|ratio|speed_down|speed_up|progress
    dir=asc|desc
    limit=<int>             default 100, max 1000
    offset=<int>
  → 200 {
      "total": int,
      "torrents": [TorrentSummary]
    }

GET /api/v1/torrents/:hash
  → 200 TorrentDetail

POST /api/v1/torrents
  body (multipart/form-data):
    torrent=<file>          .torrent file (optional if magnet provided)
    magnet=<string>         magnet URI (optional if torrent provided)
    category=<string>
    tags=<comma-separated>
    save_path=<string>
    start=<bool>            default true
    skip_checking=<bool>    default false
  → 202 { "hash": str }

DELETE /api/v1/torrents/:hash
  query:
    delete_files=<bool>     default false
  → 204

POST /api/v1/torrents/:hash/start   → 204
POST /api/v1/torrents/:hash/stop    → 204
POST /api/v1/torrents/:hash/recheck → 204
POST /api/v1/torrents/:hash/reannounce → 204

PATCH /api/v1/torrents/:hash
  body (any subset):
    { "category": str, "tags": [...], "save_path": str,
      "upload_limit": int, "download_limit": int,
      "ratio_limit": float, "seeding_time_limit": int }
  → 204
```

### Files

```
GET /api/v1/torrents/:hash/files
  → 200 { "files": [FileInfo] }

PATCH /api/v1/torrents/:hash/files
  body: { "files": [{ "index": int, "priority": 0|1|2 }] }
  → 204
```

### Trackers

```
GET /api/v1/torrents/:hash/trackers
  → 200 { "trackers": [TrackerInfo] }

POST /api/v1/torrents/:hash/trackers
  body: { "urls": [str] }
  → 204

PUT /api/v1/torrents/:hash/trackers/:url
  body: { "new_url": str }
  → 204

DELETE /api/v1/torrents/:hash/trackers/:url
  → 204
```

### Bulk operations

```
POST /api/v1/bulk/start
POST /api/v1/bulk/stop
POST /api/v1/bulk/recheck
POST /api/v1/bulk/reannounce
POST /api/v1/bulk/delete
POST /api/v1/bulk/set-category
POST /api/v1/bulk/add-tags
POST /api/v1/bulk/set-save-path
POST /api/v1/bulk/replace-tracker
  body: { "hashes": [str], ...operation-specific fields }
  query:
    dry_run=true    returns preview without executing
  → 200 { "affected": int, "preview": [...] }   (dry_run)
  → 202 { "job_id": str }                        (real run)

GET /api/v1/jobs/:id   → job status and progress
```

### Categories & Tags

```
GET    /api/v1/categories         → { "categories": [...] }
POST   /api/v1/categories         → 201
PUT    /api/v1/categories/:name   → 204
DELETE /api/v1/categories/:name   → 204

GET    /api/v1/tags               → { "tags": [...] }
POST   /api/v1/tags               → 201
DELETE /api/v1/tags/:name         → 204
```

### Sync (WebSocket)

```
GET /ws
  Upgrade: websocket

Server → client messages:
  { "type": "torrent_update",  "hash": str, "fields": {...} }
  { "type": "torrent_added",   "hash": str, "summary": TorrentSummary }
  { "type": "torrent_removed", "hash": str }
  { "type": "stats",           "upload_speed": int, "download_speed": int, ... }
  { "type": "job_progress",    "job_id": str, "done": int, "total": int }
```

### Transfer stats

```
GET /api/v1/transfer
  → 200 {
      "upload_speed": int,
      "download_speed": int,
      "total_uploaded": int,
      "total_downloaded": int,
      "free_space": { "<path>": int }
    }
```

### Health & Metrics

```
GET /health
  → 200 { "status": "ok", "rtorrent": "connected", "cache_age_ms": int }
  → 503 if rTorrent unreachable

GET /metrics
  → Prometheus text format
```

---

## qBittorrent-compatible API — /api/qb/v2/

See [qBittorrent Web API v2](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29) for field-level documentation. This section documents coverage status.

### Auth
| Endpoint | Status |
|---|---|
| `POST /api/qb/v2/auth/login` | ✅ Phase 3 |
| `POST /api/qb/v2/auth/logout` | ✅ Phase 3 |

### App
| Endpoint | Status |
|---|---|
| `GET /api/qb/v2/app/version` | ✅ Phase 3 |
| `GET /api/qb/v2/app/webapiVersion` | ✅ Phase 3 |
| `GET /api/qb/v2/app/preferences` | ⚠️ Partial |

### Torrents
| Endpoint | Status |
|---|---|
| `GET  /api/qb/v2/torrents/info` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/add` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/pause` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/resume` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/delete` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/recheck` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/reannounce` | ✅ Phase 3 |
| `GET  /api/qb/v2/torrents/properties` | ✅ Phase 3 |
| `GET  /api/qb/v2/torrents/trackers` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/editTracker` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/addTrackers` | ✅ Phase 3 |
| `GET  /api/qb/v2/torrents/files` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/filePrio` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/setCategory` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/addTags` | ✅ Phase 3 |
| `POST /api/qb/v2/torrents/removeTags` | ✅ Phase 3 |
| `GET  /api/qb/v2/torrents/categories` | ✅ Phase 3 |
| `GET  /api/qb/v2/torrents/tags` | ✅ Phase 3 |

### Sync & Transfer
| Endpoint | Status |
|---|---|
| `GET /api/qb/v2/sync/maindata` | ✅ Phase 3 |
| `GET /api/qb/v2/transfer/info` | ✅ Phase 3 |

---

## Data Types

### TorrentSummary
```json
{
  "hash": "string",
  "name": "string",
  "status": "downloading|seeding|stopped|checking|error|queued",
  "size": 0,
  "progress": 0.0,
  "download_speed": 0,
  "upload_speed": 0,
  "eta": 0,
  "ratio": 0.0,
  "category": "string",
  "tags": ["string"],
  "added_on": 0,
  "save_path": "string",
  "tracker": "string",
  "num_seeds": 0,
  "num_leechs": 0,
  "uploaded": 0,
  "downloaded": 0
}
```

### TorrentDetail
Extends TorrentSummary with:
```json
{
  "comment": "string",
  "created_by": "string",
  "creation_date": 0,
  "total_size": 0,
  "pieces_num": 0,
  "piece_size": 0,
  "download_limit": 0,
  "upload_limit": 0,
  "ratio_limit": 0.0,
  "seeding_time_limit": 0,
  "completed_on": 0,
  "trackers": [TrackerInfo]
}
```

### TrackerInfo
```json
{
  "url": "string",
  "status": "working|not_working|not_contacted|disabled",
  "tier": 0,
  "num_peers": 0,
  "num_seeds": 0,
  "num_leechs": 0,
  "msg": "string",
  "last_announce": 0,
  "next_announce": 0
}
```

### FileInfo
```json
{
  "index": 0,
  "name": "string",
  "size": 0,
  "progress": 0.0,
  "priority": 0,
  "is_seed": false,
  "piece_range": [0, 0]
}
```
