# TorrentNG Mobile Compatibility Certification

- Date UTC: 2026-09-10T19:22:00Z
- TorrentNG URL: http://127.0.0.1:28180
- List limit: 50000

## Checks

| Check | Result | Detail |
|---|---|---|
| NZB360-style /api/qb/v2 login | PASS | /api/qb/v2 auth cookie accepted |
| NZB360-style /api/qb/v2 /app/version | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /app/webapiVersion | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /app/buildInfo | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /app/preferences | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /app/defaultSavePath | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /transfer/info | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /torrents/categories | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 /torrents/tags | PASS | HTTP 200 |
| NZB360-style /api/qb/v2 list | PASS | HTTP 200 rows=36 first_hash=b272e1980f3f34ff61195dcd4ffa0682902b8612 |
| NZB360-style /api/qb/v2 properties | PASS | b272e1980f3f34ff61195dcd4ffa0682902b8612 total_size=262144 |
| NZB360-style /api/qb/v2 filter filter=completed&limit=25 | PASS | rows=25 |
| NZB360-style /api/qb/v2 filter filter=paused&limit=25 | PASS | rows=0 |
| NZB360-style /api/qb/v2 filter category=cert-scale&limit=25 | PASS | rows=0 |
| NZB360-style /api/qb/v2 filter sort=ratio&reverse=true&limit=25 | PASS | rows=25 |
| NZB360-style /api/qb/v2 sync full | PASS | rid=18238 torrents=36 |
| NZB360-style /api/qb/v2 sync delta | PASS | HTTP 200 torrents=0 |
| Transdrone-style /api/v2 login | PASS | /api/v2 auth cookie accepted |
| Transdrone-style /api/v2 /app/version | PASS | HTTP 200 |
| Transdrone-style /api/v2 /app/webapiVersion | PASS | HTTP 200 |
| Transdrone-style /api/v2 /app/buildInfo | PASS | HTTP 200 |
| Transdrone-style /api/v2 /app/preferences | PASS | HTTP 200 |
| Transdrone-style /api/v2 /app/defaultSavePath | PASS | HTTP 200 |
| Transdrone-style /api/v2 /transfer/info | PASS | HTTP 200 |
| Transdrone-style /api/v2 /torrents/categories | PASS | HTTP 200 |
| Transdrone-style /api/v2 /torrents/tags | PASS | HTTP 200 |
| Transdrone-style /api/v2 list | PASS | HTTP 200 rows=36 first_hash=b272e1980f3f34ff61195dcd4ffa0682902b8612 |
| Transdrone-style /api/v2 properties | PASS | b272e1980f3f34ff61195dcd4ffa0682902b8612 total_size=262144 |
| Transdrone-style /api/v2 filter filter=completed&limit=25 | PASS | rows=25 |
| Transdrone-style /api/v2 filter filter=paused&limit=25 | PASS | rows=0 |
| Transdrone-style /api/v2 filter category=cert-scale&limit=25 | PASS | rows=0 |
| Transdrone-style /api/v2 filter sort=ratio&reverse=true&limit=25 | PASS | rows=25 |
| Transdrone-style /api/v2 sync full | PASS | rid=18238 torrents=36 |
| Transdrone-style /api/v2 sync delta | PASS | HTTP 200 torrents=0 |

Overall status: PASS
