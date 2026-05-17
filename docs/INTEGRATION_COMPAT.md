# Integration Compatibility Harness

TorrentNG has compatibility coverage for both runtime modes. The target is
universal in/out compatibility: client imports, API facades, automation flows,
and wire-level interop all need evidence before they are considered complete.

- Track 1 sidecar qBittorrent flows in `sidecar/tests/qbcompat.rs`.
- Native compatibility API projections through `rt-api-qbit`,
  `rt-api-transmission`, `rt-api-deluge`, and `rt-api-rtorrent`, included in
  `scripts/api_facade_certification.sh`.
- The broad local compatibility gate in
  `scripts/universal_compatibility_certification.sh`, which covers API facades,
  migration/fastresume, Track 1 qBit flows, native API, engine hooks, scale,
  and storage topology. Set `UNIVERSAL_COMPAT_LIVE=1` for Docker client interop
  and `UNIVERSAL_COMPAT_PUBLIC=1` for official public torrent downloads.

The build backlog and source-to-implementation comparison live in
`docs/CLIENT_COMPATIBILITY_MATRICES.md`.

## Track 1 Sidecar Flows

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

## Native Engine Compatibility

Run the native compatibility projection tests:

```bash
scripts/api_facade_certification.sh
cargo test -p rt-api-qbit -p rt-api-transmission -p rt-api-deluge -p rt-api-rtorrent
```

Run the broad local compatibility gate:

```bash
scripts/universal_compatibility_certification.sh
```

Run the full native certification gate:

```bash
scripts/native_engine_certification_report.sh
```

When a native daemon is running, bind the report to its live capability
manifest:

```bash
NATIVE_ENGINE_URL=http://127.0.0.1:8080 scripts/native_engine_certification_report.sh
```

Live certification:

```bash
TNG_HOST_URL=http://localhost:28080 ./scripts/mobile_compat_certification.sh
```

The live script exercises NZB360/Transdrone-style read flows against both qBittorrent prefixes (`/api/qb/v2` and `/api/v2`): auth, app info, preferences, transfer info, torrent list filters, properties, and full/delta `sync/maindata`.
