# Integration Compatibility Harness

The qBittorrent shim is covered by executable flow tests in `sidecar/tests/qbcompat.rs`.

Current named flows:

| Flow | Coverage |
|---|---|
| `qb_integration_flow_read_only_clients` | NZB360/Transdrone-style app/version/preferences/transfer/list/properties reads |
| `qb_integration_flow_arr_category_tag_and_sync` | Sonarr/Radarr/Prowlarr category creation, tag creation, full `sync/maindata`, tag updates, filtered list |
| `qb_integration_flow_cross_seed_tracker_and_reannounce` | cross-seed-style reannounce, qBit v5 start/stop aliases, `setAutoTMM`, add/remove trackers |
| `qb_extended_torrent_forms_parse` | add-torrent multipart fields commonly sent by *arr/autobrr clients |

Run the harness:

```bash
cd sidecar
cargo test qb_integration_flow
cargo test qb_extended_torrent_forms_parse
```

These tests run against an in-memory sidecar and do not require a live rTorrent instance. Endpoints that call rTorrent are expected to fail gracefully or return qBit-compatible success for no-op compatibility surfaces.

Live certification:

```bash
RTNG_HOST_URL=http://localhost:28080 ./scripts/mobile_compat_certification.sh
```

The live script exercises NZB360/Transdrone-style read flows against both qBittorrent prefixes (`/api/qb/v2` and `/api/v2`): auth, app info, preferences, transfer info, torrent list filters, properties, and full/delta `sync/maindata`.
