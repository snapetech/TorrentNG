# rt-api-qbit

qBittorrent Web API v2 compatibility facade over native engine state.

## Status: Implemented — compatibility facade

This crate exposes both `/api/qb/v2` and `/api/v2` routes so automation tools
can configure rtorrentNG as a qBittorrent-compatible download client.

The facade projects native torrent registry and engine metadata into qBit
response shapes. Compatibility structs are intentionally not the internal engine
model.

Covered surfaces include auth, app info, torrent list/add/control/delete,
trackers, files, categories, tags, transfer info, `sync/maindata`, RSS/search
probe endpoints, and common qBit v5 aliases.

Run focused tests:

```sh
cargo test -p rt-api-qbit
```

Run the full native compatibility gate:

```sh
scripts/native_engine_certification_report.sh
```
