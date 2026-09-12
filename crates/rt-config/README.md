# rt-config

TOML config loading and validation for the `torrentngd` TorrentNG client daemon.

## Status: Implemented — TorrentNG client support

`torrentngd` loads the first existing config path in this order:

1. `TORRENTNGD_CONFIG`
2. `~/.config/torrentngd/config.toml`
3. `/etc/torrentngd/config.toml`

The config owns TorrentNG client, network, storage, tracker, DHT, database, and
auth settings. Compatible-client service config is separate and remains under
`torrentng` / `TNG_*`.

See [../../docs/CONFIGURATION.md](../../docs/CONFIGURATION.md) for the
operator-facing config reference.
