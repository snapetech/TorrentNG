# Configuration

TorrentNG has two runtime configuration surfaces:

- `rusttorrentd`, the native engine daemon and primary runtime.
- `torrentng`, the Track 1 rTorrent sidecar used for migration and compatibility deployments.

The two config files are intentionally separate. Native config controls durable engine state, peer networking, tracker behavior, storage, DHT, and native API auth. Sidecar config controls the rTorrent SCGI bridge, sidecar cache, WebUI serving, and qBittorrent-compatible facade identity.

## Native daemon

`rusttorrentd` loads TOML config from the first existing path in this order:

1. `RUSTTORRENTD_CONFIG`
2. `~/.config/rusttorrentd/config.toml`
3. `/etc/rusttorrentd/config.toml`

If no file exists, built-in defaults are used.

Start with an explicit config path:

```sh
RUSTTORRENTD_CONFIG=/config/config.toml rusttorrentd
```

### `[daemon]`

| Key | Default | Description |
|---|---|---|
| `session_dir` | `~/.local/share/rusttorrentd` or `/var/lib/rusttorrentd` | Directory for torrent metadata and session state |
| `api_bind` | `127.0.0.1:8080` | Bind address for the REST and compatibility APIs |
| `log_level` | `info` | Tracing filter, for example `info` or `rt_engine=trace` |
| `shutdown_timeout_secs` | `10` | Max seconds to wait for torrent tasks to send stopped announces during shutdown |

### `[network]`

| Key | Default | Description |
|---|---|---|
| `listen_port` | `6881` | Incoming peer TCP port |
| `max_peers` | `200` | Maximum peer connections across all torrents |
| `upload_rate_limit` | `0` | Upload limit in bytes/sec; `0` means unlimited |
| `download_rate_limit` | `0` | Download limit in bytes/sec; `0` means unlimited |

### `[storage]`

| Key | Default | Description |
|---|---|---|
| `download_dir` | `~/Downloads` or `/tmp` | Default payload download directory |
| `device_elevator_enabled` | `true` | Enable per-device peer-read elevator scheduling where storage profiles benefit |

### `[memory]`

| Key | Default | Description |
|---|---|---|
| `total_cap_mb` | `512` | Process-owned memory cap for governor-managed buffers |
| `storage_frame_cap_mb` | `128` | Storage frame memory class cap |
| `piece_assembly_cap_mb` | `128` | Incomplete piece assembly memory class cap |
| `peer_buffer_cap_mb` | `128` | Peer rx/tx and webseed buffer memory class cap |
| `metadata_cap_mb` | `32` | Metadata, tracker peer cache, DHT table, and API snapshot class baseline cap |
| `pressure_constrained_pct` | `75` | Percent of total cap that reports constrained pressure |
| `pressure_critical_pct` | `90` | Percent of total cap that reports critical pressure |

### `[runtime]`

| Key | Default | Description |
|---|---|---|
| `torrent_tiers_enabled` | `true` | Keep idle torrents in Dormant/Warm tiers so only active torrents own task/runtime resources |

### `[tracker]`

| Key | Default | Description |
|---|---|---|
| `http_timeout_secs` | `30` | HTTP announce timeout |
| `udp_timeout_secs` | `15` | UDP announce timeout |
| `min_interval_secs` | `0` | Minimum announce interval override; `0` uses tracker values |

### `[dht]`

| Key | Default | Description |
|---|---|---|
| `enabled` | `true` | Enable DHT |
| `port` | `0` | UDP DHT port; `0` uses `network.listen_port` |
| `bootstrap_nodes` | Public BitTorrent bootstrap routers | Bootstrap nodes as `host:port` strings |

### `[db]`

| Key | Default | Description |
|---|---|---|
| `path` | `session_dir/state.db` | SQLite database path; leave empty to use the session directory |
| `wal_checkpoint_pages` | `1000` | SQLite WAL checkpoint threshold |

### `[auth]`

| Key | Default | Description |
|---|---|---|
| `api_tokens` | `[]` | Pre-shared bearer/session tokens accepted by the native API |

### Native minimal example

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/rusttorrentd"

[storage]
download_dir = "/data"

[auth]
api_tokens = ["change-me"]
```

### Native full example

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/rusttorrentd"
log_level = "info"
shutdown_timeout_secs = 10

[network]
listen_port = 6881
max_peers = 200
upload_rate_limit = 0
download_rate_limit = 0

[storage]
download_dir = "/data"

[tracker]
http_timeout_secs = 30
udp_timeout_secs = 15
min_interval_secs = 0

[dht]
enabled = true
port = 0
bootstrap_nodes = [
  "dht.transmissionbt.com:6881",
  "router.bittorrent.com:6881",
  "router.utorrent.com:6881",
]

[db]
path = "/var/lib/rusttorrentd/state.db"
wal_checkpoint_pages = 1000

[auth]
api_tokens = ["your-automation-token"]
```

## Track 1 sidecar

The sidecar loads TOML config from `~/.config/torrentng/config.toml` by default. Override the path by passing it as the first argument:

```sh
torrentng /path/to/config.toml
```

Environment variables override file values where listed.

### Top-level sidecar options

| Key | Default | Env override | Description |
|---|---|---|---|
| `listen_addr` | `0.0.0.0:8080` | `TNG_LISTEN_ADDR` | TCP address the sidecar listens on |
| `debug` | `false` | `TNG_DEBUG=1` | Enable debug logging |
| `sync_interval_secs` | `2` | - | Seconds between rTorrent state polls |
| `data_dir` | `~/.local/share/torrentng` | - | Directory for SQLite cache |
| `storage_roots` | `[]` | - | Paths shown in the storage dashboard; defaults to `/` when empty |
| WebUI static dir | `static` | `TNG_STATIC_DIR` | Directory served for WebUI assets and SPA fallback |

### Sidecar `[rtorrent]`

| Key | Default | Env override | Description |
|---|---|---|---|
| `scgi_socket` | `/run/rtorrent/rpc.sock` | `TNG_SCGI_SOCKET` | Path to rTorrent SCGI Unix socket |
| `scgi_addr` | - | `TNG_SCGI_ADDR` | `host:port` for TCP SCGI; mutually exclusive with `scgi_socket` |
| `timeout_secs` | `10` | - | Timeout for individual XMLRPC calls |
| `user_agent` | `rtorrent/0.16.11` | `TNG_USER_AGENT` | Client identifier pushed to rTorrent on startup |

#### `user_agent`

The `user_agent` value is pushed to rTorrent via `network.http.user_agent.set` on startup when the running rTorrent build exposes that XMLRPC method. TorrentNG's packaged rTorrent 0.16.11 image carries a small build patch that publishes the existing libtorrent HTTP user-agent getter/setter through XMLRPC, so this works in the Docker and certification builds. It can also be changed at runtime via `PUT /api/v1/settings/user-agent` or the Settings panel in the WebUI. Some older distro rTorrent packages do not expose tracker user-agent control; the certification harness reports that as blocked instead of assuming spoofing works.

```toml
[rtorrent]
user_agent = "rtorrent/0.16.11"
```

```sh
TNG_USER_AGENT="rtorrent/0.16.11" torrentng
```

Known values:

| Client | User-agent string |
|---|---|
| rTorrent 0.16.11 | `rtorrent/0.16.11` |
| libtorrent 0.16.11 | `libtorrent/0.16.11` |
| qBittorrent 5.0.0 | `qBittorrent/5.0.0` |
| Deluge 2.2.0 | `Deluge/2.2.0 libtorrent/2.0.10` |
| Transmission 4.0 | `Transmission/4.0` |

### Sidecar `[identity]`

These values control the qBittorrent-compatible API identity presented to automation clients. They do not change the tracker-facing BitTorrent engine identity.

| Key | Default | Env override | Description |
|---|---|---|---|
| `qbittorrent_version` | `5.0.0` | `TNG_QBITTORRENT_VERSION` | Response for `/api/v2/app/version` |
| `qbittorrent_webapi_version` | `2.11.0` | `TNG_QBITTORRENT_WEBAPI_VERSION` | Response for `/api/v2/app/webapiVersion` |
| `qbittorrent_build_libtorrent` | `0.16.11` | `TNG_QBITTORRENT_BUILD_LIBTORRENT` | `libtorrent` value in `/api/v2/app/buildInfo` |
| `qbittorrent_build_qt` | `6.7.0` | `TNG_QBITTORRENT_BUILD_QT` | `qt` value in `/api/v2/app/buildInfo` |

Use these only for lab compatibility testing. Tracker-facing identity is still controlled by the underlying rTorrent build and `TNG_USER_AGENT` support.

### Sidecar `[auth]`

| Key | Default | Env override | Description |
|---|---|---|---|
| `secret_key` | - | `TNG_SECRET_KEY` | Secret for signing session tokens. Set in production. |
| `api_tokens` | `[]` | `TNG_API_TOKENS` | Comma-separated pre-shared bearer tokens for automation tools |
| `trust_proxy_header` | `false` | - | Trust `X-Remote-User` from reverse proxy |

### Sidecar `[workflows]`

| Key | Default | Env override | Description |
|---|---|---|---|
| `allow_scripts` | `false` | `TNG_ALLOW_SCRIPTS=1` | Enables workflow `script` actions |
| `script_timeout_secs` | `30` | - | Maximum runtime for each script action |
| `allowed_script_dirs` | `[]` | - | Optional canonical directory allowlist for script paths |

Script actions are refused unless `allow_scripts` is true. Production configs that enable scripts should set `allowed_script_dirs` to root-owned or service-owned directories and keep those directories non-world-writable. See [SECURITY_REVIEW.md](SECURITY_REVIEW.md).

### Sidecar minimal example

```toml
[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
```

### Sidecar container example

See [deploy/docker/sidecar.config.toml](../deploy/docker/sidecar.config.toml) for the Phase 1 container-oriented sidecar config.

### Sidecar full example

```toml
listen_addr = "127.0.0.1:8080"
debug = false
sync_interval_secs = 2
data_dir = "/var/lib/torrentng"
storage_roots = ["/data", "/mnt/archive"]

[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
timeout_secs = 10
user_agent = "rtorrent/0.16.11"

[auth]
secret_key = "change-me-in-production"
api_tokens = ["your-autobrr-token", "your-prowlarr-token"]
trust_proxy_header = false

[identity]
qbittorrent_version = "5.0.0"
qbittorrent_webapi_version = "2.11.0"
qbittorrent_build_libtorrent = "0.16.11"
qbittorrent_build_qt = "6.7.0"

[workflows]
allow_scripts = false
script_timeout_secs = 30
allowed_script_dirs = []
```
