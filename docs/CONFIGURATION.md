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

---

## `[rtorrent]` section

| Key | Default | Env override | Description |
|---|---|---|---|
| `scgi_socket` | `/run/rtorrent/rpc.sock` | `RTNG_SCGI_SOCKET` | Path to rTorrent SCGI Unix socket |
| `scgi_addr` | — | `RTNG_SCGI_ADDR` | `host:port` for TCP SCGI (mutually exclusive with `scgi_socket`) |
| `timeout_secs` | `10` | — | Timeout for individual XMLRPC calls |
| `user_agent` | `rtorrentNG/0.1.0 libtorrent/0.16.11` | `RTNG_USER_AGENT` | Client identifier pushed to rTorrent on startup |

### `user_agent`

The `user_agent` value is pushed to rTorrent via `network.http.user_agent.set` on startup. It can also be changed at runtime via `PUT /api/v1/settings/user-agent` or the Settings panel in the WebUI.

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

## `[auth]` section

| Key | Default | Description |
|---|---|---|
| `secret_key` | — | Secret for signing session tokens. Set in production. |
| `api_tokens` | `[]` | Pre-shared bearer tokens for automation tools |
| `trust_proxy_header` | `false` | Trust `X-Remote-User` from reverse proxy |

---

## Minimal example

```toml
[rtorrent]
scgi_socket = "/run/rtorrent/rpc.sock"
```

## Full example

```toml
listen_addr      = "127.0.0.1:8080"
debug            = false
sync_interval_secs = 2
data_dir         = "/var/lib/rtorrentng"

[rtorrent]
scgi_socket  = "/run/rtorrent/rpc.sock"
timeout_secs = 10
user_agent   = "rtorrentNG/0.1.0 libtorrent/0.16.11"

[auth]
secret_key        = "change-me-in-production"
api_tokens        = ["your-autobrr-token", "your-prowlarr-token"]
trust_proxy_header = false
```
