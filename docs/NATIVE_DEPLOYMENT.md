# Native Engine Deployment

Native mode runs `rusttorrentd` as the source of truth. qBittorrent,
Transmission, Deluge, and legacy UI compatibility surfaces are facades over the
same durable engine state.

## Production Requirements

- Put the session DB and torrent metadata on durable local storage.
- Put payload data on mounted storage roots with stable paths.
- Set native API tokens in `[auth].api_tokens`; do not use example secrets.
- Bind the native API behind TLS or a trusted reverse proxy.
- Keep mutating endpoints token-protected.
- Enable scripts only with a root-owned allowlist directory.
- Run backup before imports, bulk moves, or upgrades.

## Config

`rusttorrentd` loads config from `RUSTTORRENTD_CONFIG`, then
`~/.config/rusttorrentd/config.toml`, then `/etc/rusttorrentd/config.toml`.
When no config exists, defaults are used.

Minimal production shape:

```toml
[daemon]
api_bind = "127.0.0.1:8080"
session_dir = "/var/lib/rusttorrentd"

[storage]
download_dir = "/data"

[auth]
api_tokens = ["change-me"]
```

The daemon stores SQLite state at `session_dir/state.db` unless `[db].path` is
set explicitly. See [CONFIGURATION.md](CONFIGURATION.md) for the full native
config surface.

## Start

```sh
RUSTTORRENTD_CONFIG=/config/config.toml rusttorrentd
```

Minimum validation:

```sh
curl -fsS http://127.0.0.1:8080/health
curl -fsS http://127.0.0.1:8080/api/v1/torrents
curl -fsS http://127.0.0.1:8080/api/qb/v2/torrents/info
```

## Upgrade

1. Run `scripts/native_engine_certification_report.sh` on the current build.
2. Back up session state with [BACKUP_RESTORE.md](BACKUP_RESTORE.md).
3. Deploy the new binary/container.
4. Verify `/health`, native list, qBit list, and metrics.
5. Keep the previous binary/container image until restart recovery is confirmed.

## Certification

The native release gate is:

```sh
scripts/native_engine_certification_report.sh
```

When a daemon is running, bind the certification report to the live `/health`
capability manifest as well:

```sh
NATIVE_ENGINE_URL=http://127.0.0.1:8080 scripts/native_engine_certification_report.sh
```

The post-soak release gate also reruns native engine rewrite certification
directly before refreshing the aggregate release report.

Live public transfer evidence is optional for offline CI but required before a
production release:

```sh
scripts/public_linux_iso_certification.sh
```
