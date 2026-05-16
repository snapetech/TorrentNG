# Configuration

Configuration is loaded from a TOML file. Environment variables override any file value.

Default config path: `~/.config/rtorrentng/config.toml`

Override path by passing it as the first argument: `rtorrentng /path/to/config.toml`

---

## Top-level options

| Key | Default | Env override | Description |
|---|---|---|---|
| `listen_addr` | `0.0.0.0:8080` | `RTNG_LISTEN_ADDR` | TCP address the sidecar listens on |
| `debug` | `false` | `RTNG_DEBUG=1` | Enable debug logging |
| `sync_interval_secs` | `2` | — | Seconds between rTorrent state polls |
| `data_dir` | `~/.local/share/rtorrentng` | — | Directory for SQLite cache |
| `storage_roots` | `[]` | — | Paths shown in the storage dashboard; defaults to `/` when empty |
| WebUI static dir | `static` | `RTNG_STATIC_DIR` | Directory served for WebUI assets and SPA fallback |

---

## `[rtorrent]` section

| Key | Default | Env override | Description |
|---|---|---|---|
| `scgi_socket` | `/run/rtorrent/rpc.sock` | `RTNG_SCGI_SOCKET` | Path to rTorrent SCGI Unix socket |
| `scgi_addr` | — | `RTNG_SCGI_ADDR` | `host:port` for TCP SCGI (mutually exclusive with `scgi_socket`) |
| `timeout_secs` | `10` | — | Timeout for individual XMLRPC calls |
| `user_agent` | `rtorrentNG/0.1.0 libtorrent/0.16.11` | `RTNG_USER_AGENT` | Client identifier pushed to rTorrent on startup |

### `user_agent`

The `user_agent` value is pushed to rTorrent via `network.http.user_agent.set` on startup when the running rTorrent build exposes that XMLRPC method. rtorrentNG's packaged rTorrent 0.16.11 image carries a small build patch that publishes the existing libtorrent HTTP user-agent getter/setter through XMLRPC, so this works in the Docker and certification builds. It can also be changed at runtime via `PUT /api/v1/settings/user-agent` or the Settings panel in the WebUI. Some older distro rTorrent packages do not expose tracker user-agent control; the certification harness reports that as blocked instead of assuming spoofing works.

**Config file:**
```toml
[rtorrent]
user_agent = "rtorrentNG/0.1.0 libtorrent/0.16.11"
```

**Environment variable:**
```sh
RTNG_USER_AGENT="rtorrentNG/0.1.0 libtorrent/0.16.11" rtorrentng
```

**Known values:**

| Client | User-agent string |
|---|---|
| rTorrent 0.16.11 | `rtorrent/0.16.11` |
| libtorrent 0.16.11 | `libtorrent/0.16.11` |
| qBittorrent 5.0.0 | `qBittorrent/5.0.0` |
| Deluge 2.2.0 | `Deluge/2.2.0 libtorrent/2.0.10` |
| Transmission 4.0 | `Transmission/4.0` |

The default `rtorrentNG/0.1.0 libtorrent/0.16.11` is used in packaged releases.

---

## `[identity]` section

These values control the qBittorrent-compatible API identity presented to automation clients. They do not change the tracker-facing BitTorrent engine identity.

| Key | Default | Env override | Description |
|---|---|---|---|
| `qbittorrent_version` | `5.0.0` | `RTNG_QBITTORRENT_VERSION` | Response for `/api/v2/app/version` |
| `qbittorrent_webapi_version` | `2.11.0` | `RTNG_QBITTORRENT_WEBAPI_VERSION` | Response for `/api/v2/app/webapiVersion` |
| `qbittorrent_build_libtorrent` | `0.16.11` | `RTNG_QBITTORRENT_BUILD_LIBTORRENT` | `libtorrent` value in `/api/v2/app/buildInfo` |
| `qbittorrent_build_qt` | `6.7.0` | `RTNG_QBITTORRENT_BUILD_QT` | `qt` value in `/api/v2/app/buildInfo` |

Use these only for lab compatibility testing. Tracker-facing identity is still controlled by the underlying rTorrent build and `RTNG_USER_AGENT` support.

---

## `[auth]` section

| Key | Default | Env override | Description |
|---|---|---|---|
| `secret_key` | — | `RTNG_SECRET_KEY` | Secret for signing session tokens. Set in production. |
| `api_tokens` | `[]` | `RTNG_API_TOKENS` | Comma-separated pre-shared bearer tokens for automation tools |
| `trust_proxy_header` | `false` | — | Trust `X-Remote-User` from reverse proxy |

---

## `[workflows]` section

| Key | Default | Env override | Description |
|---|---|---|---|
| `allow_scripts` | `false` | `RTNG_ALLOW_SCRIPTS=1` | Enables workflow `script` actions |
| `script_timeout_secs` | `30` | — | Maximum runtime for each script action |
| `allowed_script_dirs` | `[]` | — | Optional canonical directory allowlist for script paths |

Script actions are refused unless `allow_scripts` is true. Production configs that enable scripts should set `allowed_script_dirs` to root-owned or service-owned directories and keep those directories non-world-writable. See [docs/SECURITY_REVIEW.md](/home/keith/Documents/code/rtorrentNG/docs/SECURITY_REVIEW.md).

---

## Minimal example

```toml
[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
```

## Container example

See [deploy/docker/sidecar.config.toml](/home/keith/Documents/code/rtorrentNG/deploy/docker/sidecar.config.toml) for the Phase 1 container-oriented sidecar config.

## Full example

```toml
listen_addr      = "127.0.0.1:8080"
debug            = false
sync_interval_secs = 2
data_dir         = "/var/lib/rtorrentng"
storage_roots   = ["/data", "/mnt/archive"]

[rtorrent]
scgi_socket  = "/run/rtorrent/rpc.sock"
timeout_secs = 10
user_agent   = "rtorrentNG/0.1.0 libtorrent/0.16.11"

[auth]
secret_key        = "change-me-in-production"
api_tokens        = ["your-autobrr-token", "your-prowlarr-token"]
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
