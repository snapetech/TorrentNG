# rt-config

TOML config loading and validation for the native `rusttorrentd` daemon.

## Status: Implemented — native engine support

`rusttorrentd` loads the first existing config path in this order:

1. `RUSTTORRENTD_CONFIG`
2. `~/.config/rusttorrentd/config.toml`
3. `/etc/rusttorrentd/config.toml`

The config owns native daemon, network, storage, tracker, DHT, database, and
auth settings. Track 1 sidecar config is separate and remains under
`rtorrentng` / `RTNG_*`.

See [../../docs/CONFIGURATION.md](../../docs/CONFIGURATION.md) for the
operator-facing config reference.
