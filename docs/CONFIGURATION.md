# Configuration

TorrentNG has two runtime configuration surfaces:

- `torrentngd`, the native engine daemon and primary runtime.
- `torrentng`, the sidecar WebUI/API control plane used with rTorrent, qBittorrent, Transmission, Deluge, or a separate TorrentNG native daemon.

The two config files are intentionally separate. Native config controls durable engine state, peer networking, tracker behavior, storage, DHT, and native API auth. Sidecar config controls the selected backend adapter, sidecar cache, WebUI serving, and qBittorrent-compatible facade identity.

## Native daemon

`torrentngd` loads TOML config from the first existing path in this order:

1. `TORRENTNGD_CONFIG`
2. `~/.config/torrentngd/config.toml`
3. `/etc/torrentngd/config.toml`

If no file exists, built-in defaults are used.

Start with an explicit config path:

```sh
TORRENTNGD_CONFIG=/config/config.toml torrentngd
```

### `[daemon]`

| Key | Default | Description |
|---|---|---|
| `session_dir` | `~/.local/share/torrentngd` or `/var/lib/torrentngd` | Directory for torrent metadata and session state |
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

uTP transport policy is controlled by environment so operators can roll it out
without changing durable config. By default, `TNG_UTP_OUTGOING=auto` attempts
uTP first for DHT, PEX, and manually added peers while tracker-discovered peers
stay on TCP. `TNG_UTP_OUTGOING=prefer|only` broadens outbound uTP peer-wire
dialing (`prefer` falls back to TCP, `only` does not), and
`TNG_UTP_OUTGOING=tcp-only` disables outbound uTP. `TNG_UTP_INCOMING=1`
binds the shared UDP incoming uTP endpoint on `listen_port`; the incoming
listener flag is boolean only (`1`, `true`, `yes`, or `on`) and does not accept
policy words such as `prefer` or `utp-only`. `TNG_UTP_METADATA=prefer|only`
enables uTP magnet metadata fetch explicitly.

### `[storage]`

| Key | Default | Description |
|---|---|---|
| `download_dir` | `~/Downloads` or `/tmp` | Default payload download directory |
| `device_elevator_enabled` | `true` | Enable per-device peer-read elevator scheduling where storage profiles benefit |
| `file_pool_size` | `512` | Open-file cache entries per scheduler |
| `idle_file_ttl_secs` | `300` | Seconds before idle cached file handles are eligible to close |
| `io_worker_threads` | `4` | Dedicated positioned-I/O worker threads per scheduler |
| `io_queue_depth` | `256` | Bounded positioned-I/O queue depth per scheduler |
| `hash_worker_threads` | `2` | Dedicated storage hash worker threads per scheduler |
| `hash_queue_depth` | `256` | Bounded hash queue depth per scheduler |
| `preallocation_mode` | `auto` | Payload preallocation mode: `off`, `auto`, `sparse`, or `full` |
| `durability_mode` | `checkpoint` | Payload durability mode: `fast`, `checkpoint`, or `strict` |
| `peer_read_readahead_bytes` | `524288` | Peer-read readahead size used before returning the exact requested slice |
| `peer_read_cache_entries` | `64` | Bounded per-scheduler peer-read readahead cache entries; set to `0` to disable cached readahead reuse |
| `peer_read_elevator_budget_ms` | `25` | HDD/network peer-read elevator batching window; ignored when `device_elevator_enabled = false` |

### `[memory]`

| Key | Default | Description |
|---|---|---|
| `total_cap_mb` | `512` | Process-owned memory cap for governor-managed buffers |
| `storage_frame_cap_mb` | `128` | Storage frame memory class cap |
| `queued_disk_cap_mb` | `64` | Queued disk/hash/elevator memory class cap |
| `piece_assembly_cap_mb` | `128` | Incomplete piece assembly memory class cap |
| `peer_buffer_cap_mb` | `128` | Peer rx/tx and webseed buffer memory class cap |
| `metadata_cap_mb` | `32` | Metadata, tracker peer cache, DHT table, and API snapshot class baseline cap |
| `pressure_constrained_pct` | `75` | Percent of total cap that reports constrained pressure |
| `pressure_critical_pct` | `90` | Percent of total cap that reports critical pressure |

The `queued_disk` memory class reports payload bytes reserved by queued or
active disk, hash, and peer-read elevator jobs. Leases are acquired before
enqueue and released on queue rejection, cancellation, or completion.

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

### `[logging]`

`[daemon].log_level` remains supported as the legacy filter. The structured
logging section takes precedence when `filter` is set, and `RUST_LOG` takes
precedence over both.

| Key | Default | Description |
|---|---|---|
| `format` | `json` | Output format: `json` or `pretty` |
| `profile` | `basic` | Preset filter profile: `basic`, `detailed`, or `verbose` |
| `filter` | `""` | Explicit tracing filter, for example `rt_engine=debug,tower_http=info` |
| `event_retention` | `10000` | Number of newest durable session events to retain for qBit-compatible main logs |

### Native minimal example

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/torrentngd"

[storage]
download_dir = "/data"

[auth]
api_tokens = ["change-me"]
```

### Native full example

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/torrentngd"
log_level = "info"
shutdown_timeout_secs = 10

[network]
listen_port = 6881
max_peers = 200
upload_rate_limit = 0
download_rate_limit = 0

[storage]
download_dir = "/data"
preallocation_mode = "auto"
durability_mode = "checkpoint"
peer_read_cache_entries = 64

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
path = "/var/lib/torrentngd/state.db"
wal_checkpoint_pages = 1000

[auth]
api_tokens = ["your-automation-token"]

[logging]
format = "json"
profile = "basic"
filter = ""
event_retention = 10000
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
| `sync_interval_secs` | `2` | `TNG_SYNC_INTERVAL_SECS` | Seconds between backend state polls |
| `data_dir` | `~/.local/share/torrentng` | - | Directory for SQLite cache |
| `storage_roots` | `[]` | - | Paths shown in the storage dashboard; defaults to `/` when empty |
| WebUI static dir | `static` | `TNG_STATIC_DIR` | Directory served for WebUI assets and SPA fallback |

### Sidecar `[backend]`

`[backend]` selects the BitTorrent client controlled by the sidecar. Existing configs that only define `[rtorrent]` still load as rTorrent-backed deployments.

| Key | Default | Env override | Description |
|---|---|---|---|
| `type` | `rtorrent` | `TNG_BACKEND` | Backend adapter: `rtorrent`, `qbittorrent`, `transmission`, `deluge`, or `torrentng`. |

```toml
[backend]
type = "rtorrent"
```

```toml
[backend]
type = "qbittorrent"
```

### Sidecar `[rtorrent]`

| Key | Default | Env override | Description |
|---|---|---|---|
| `scgi_socket` | `/run/rtorrent/rpc.sock` | `TNG_SCGI_SOCKET` | Path to rTorrent SCGI Unix socket |
| `scgi_addr` | - | `TNG_SCGI_ADDR` | `host:port` for TCP SCGI; mutually exclusive with `scgi_socket` |
| `timeout_secs` | `10` | - | Timeout for individual XMLRPC calls |
| `user_agent` | `rtorrent/0.16.11` | `TNG_USER_AGENT` | Client identifier pushed to rTorrent on startup |

### Sidecar `[qbittorrent]`

The qBittorrent backend talks to qBittorrent-nox through the qBittorrent Web API. The sidecar keeps the TorrentNG WebUI and compatibility API in front while qBittorrent owns torrent execution.

| Key | Default | Env override | Description |
|---|---|---|---|
| `url` | `http://127.0.0.1:8080` | `TNG_QBITTORRENT_URL` | Base URL for qBittorrent Web API |
| `username` | - | `TNG_QBITTORRENT_USERNAME` | Optional WebUI username |
| `password` | - | `TNG_QBITTORRENT_PASSWORD` | Optional WebUI password |
| `timeout_secs` | `10` | - | Timeout for qBittorrent Web API requests |
| `no_auth` | `false` | `TNG_QBITTORRENT_NO_AUTH=1` | Skip login for trusted no-auth local WebUI deployments |
| `accept_invalid_certs` | `false` | - | Accept invalid TLS certificates for lab deployments |

```toml
[backend]
type = "qbittorrent"

[qbittorrent]
url = "http://127.0.0.1:8080"
username = "admin"
password = "adminadmin"
timeout_secs = 10
```

### Sidecar `[transmission]`

The Transmission backend talks to an external Transmission RPC endpoint. Categories are mapped to Transmission labels where available. File priority, tracker add/edit/remove, pause/resume, remove, add, recheck, location moves, file rename, and share limits are mapped to Transmission RPC where supported; tags, torrent rename, sequential toggles, and runtime user-agent changes are unsupported.

| Key | Default | Env override | Description |
|---|---|---|---|
| `url` | `http://127.0.0.1:9091/transmission/rpc` | `TNG_TRANSMISSION_URL` | Transmission RPC URL |
| `username` | - | `TNG_TRANSMISSION_USERNAME` | Optional RPC username |
| `password` | - | `TNG_TRANSMISSION_PASSWORD` | Optional RPC password |
| `timeout_secs` | `10` | - | Timeout for Transmission RPC requests |
| `accept_invalid_certs` | `false` | - | Accept invalid TLS certificates for lab deployments |

```toml
[backend]
type = "transmission"

[transmission]
url = "http://127.0.0.1:9091/transmission/rpc"
```

### Sidecar `[deluge]`

The Deluge backend talks to the Deluge Web JSON-RPC endpoint. File priority, tracker replacement, pause/resume, remove, add, recheck, storage moves, file rename, ratio share limits, and seeding-time share limits are mapped to Deluge core methods where supported; categories, tags, torrent rename, sequential toggles, and runtime user-agent changes are unsupported.

| Key | Default | Env override | Description |
|---|---|---|---|
| `url` | `http://127.0.0.1:8112/json` | `TNG_DELUGE_URL` | Deluge Web JSON-RPC URL |
| `password` | - | `TNG_DELUGE_PASSWORD` | Optional Deluge Web password |
| `timeout_secs` | `10` | - | Timeout for Deluge JSON-RPC requests |
| `accept_invalid_certs` | `false` | - | Accept invalid TLS certificates for lab deployments |

```toml
[backend]
type = "deluge"

[deluge]
url = "http://127.0.0.1:8112/json"
password = "deluge"
```

### Sidecar `[torrentng]`

The TorrentNG backend talks to a native TorrentNG daemon over its native HTTP API. This is primarily for deployments that want the sidecar WebUI/API compatibility layer in front of a separate native daemon. The adapter forwards torrent add/remove, pause/resume, recheck/reannounce, category/tag changes, location/name updates, file-priority and file-rename changes, and tracker add/edit/remove operations to the native daemon; sidecar-only catalog metadata such as saved views and RSS rules remains in the sidecar cache.

| Key | Default | Env override | Description |
|---|---|---|---|
| `url` | `http://127.0.0.1:8080` | `TNG_TORRENTNG_URL` | Base URL for the native daemon |
| `api_token` | - | `TNG_TORRENTNG_API_TOKEN` | Optional bearer token for mutation endpoints |
| `timeout_secs` | `10` | - | Timeout for native API requests |
| `accept_invalid_certs` | `false` | - | Accept invalid TLS certificates for lab deployments |

```toml
[backend]
type = "torrentng"

[torrentng]
url = "http://127.0.0.1:8080"
api_token = "optional-token"
```

### Sidecar `[rtorrent.logs]`

When enabled, the sidecar tails configured rTorrent log files and stores new lines as durable `rtorrent_log` app events. These entries are returned by qBittorrent-compatible `/api/v2/log/main` alongside sidecar app events. Ingestion failures and recovery are also durable operator events (`rtorrent_log_ingest_error` and `rtorrent_log_ingest_recovered`) so a broken log path is visible in the same log stream. Startup-time rTorrent connectivity failures, transfer-stat probe failures, and their recoveries are retained as operator events as well. Admin settings mutations and restart requests are retained without storing full config bodies, user-agent strings, or filesystem paths. Ingested lines and ingest errors are redacted before storage: magnet URIs, common token query parameters, cookies, and full filesystem paths are removed or shortened.

By default, first-time ingestion starts at the end of each file to avoid flooding `/log/main` with old logs. The sidecar persists per-file offsets in its cache DB, so subsequent restarts continue from the last ingested byte and capture lines written while the sidecar was down. Set `read_from_start = true` only for controlled imports.

| Key | Default | Env override | Description |
|---|---|---|---|
| `enabled` | `false` | `TNG_RTORRENT_LOGS_ENABLED` | Enable rTorrent log file ingestion |
| `paths` | `[]` | `TNG_RTORRENT_LOG_PATHS` | Log files to tail; env value is comma-separated |
| `poll_interval_secs` | `2` | `TNG_RTORRENT_LOG_POLL_INTERVAL_SECS` | Seconds between file polls |
| `read_from_start` | `false` | `TNG_RTORRENT_LOG_READ_FROM_START` | Read existing file contents on startup instead of only new lines |

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

### Sidecar `[logging]`

`debug = true` and `TNG_DEBUG=1` remain supported as legacy aliases for debug-level logging. `RUST_LOG` has the highest precedence, followed by `TNG_LOG_FILTER` or `logging.filter`, then `logging.profile`, then the legacy debug setting.

| Key | Default | Env override | Description |
|---|---|---|---|
| `format` | `json` | `TNG_LOG_FORMAT` | Output format: `json` or `pretty` |
| `profile` | `basic` | `TNG_LOG_PROFILE` | Preset filter profile: `basic`, `detailed`, or `verbose` |
| `filter` | `""` | `TNG_LOG_FILTER` | Explicit tracing filter, for example `torrentng=debug,tower_http=info` |
| `event_retention` | `10000` | `TNG_LOG_EVENT_RETENTION` | Number of newest durable sidecar app events to retain for qBit-compatible main logs |

### Sidecar `[workflows]`

| Key | Default | Env override | Description |
|---|---|---|---|
| `allow_scripts` | `false` | `TNG_ALLOW_SCRIPTS=1` | Enables workflow `script` actions |
| `script_timeout_secs` | `30` | - | Maximum runtime for each script action |
| `allowed_script_dirs` | `[]` | - | Optional canonical directory allowlist for script paths |

Script actions are refused unless `allow_scripts` is true. Production configs that enable scripts should set `allowed_script_dirs` to root-owned or service-owned directories and keep those directories non-world-writable. See [SECURITY_REVIEW.md](SECURITY_REVIEW.md).

### Sidecar minimal example

```toml
[backend]
type = "rtorrent"

[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
```

### Sidecar container example

See [deploy/docker/sidecar.config.toml](../deploy/docker/sidecar.config.toml) for the Phase 1 container-oriented sidecar config.

The Docker compose stack also includes a qBittorrent profile:

```sh
docker compose -f deploy/docker/compose.yml --profile qbittorrent up torrentng-qbittorrent qbittorrent
docker compose -f deploy/docker/compose.yml --profile transmission up torrentng-transmission transmission
docker compose -f deploy/docker/compose.yml --profile deluge up torrentng-deluge deluge
```

Those profiles expose TorrentNG on host ports `8082`, `8083`, and `8084` respectively. Native client WebUIs remain exposed on their usual profile ports for troubleshooting.

### Sidecar full example

```toml
listen_addr = "127.0.0.1:8080"
debug = false
sync_interval_secs = 2
data_dir = "/var/lib/torrentng"
storage_roots = ["/data", "/mnt/archive"]

[backend]
type = "rtorrent"

[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
timeout_secs = 10
user_agent = "rtorrent/0.16.11"

[rtorrent.logs]
enabled = false
paths = ["/var/log/rtorrent/rtorrent.log"]
poll_interval_secs = 2
read_from_start = false

[auth]
secret_key = "change-me-in-production"
api_tokens = ["your-autobrr-token", "your-prowlarr-token"]
trust_proxy_header = false

[identity]
qbittorrent_version = "5.0.0"
qbittorrent_webapi_version = "2.11.0"
qbittorrent_build_libtorrent = "0.16.11"
qbittorrent_build_qt = "6.7.0"

[logging]
format = "json"
profile = "basic"
filter = ""
event_retention = 10000

[workflows]
allow_scripts = false
script_timeout_secs = 30
allowed_script_dirs = []
```
