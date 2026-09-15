# TorrentNG Engine (`torrentngd`)

The TorrentNG Engine is TorrentNG's built-in native Rust BitTorrent client. It
wires the client engine crates, handles signals, startup, and shutdown.

## Status: Implemented — first-party client in active hardening

`torrentngd` runs the TorrentNG Rust client, TorrentNG REST/SSE API, qBittorrent
compatibility facade, Transmission facade, peer listener, tracker manager,
DHT task, durable SQLite session state, and bounded clean shutdown. It does not
require a compatible external client or rTorrent XMLRPC path.

`GET /health` exposes a TorrentNG-client capability manifest covering v1/v2/hybrid
identity, `btih`/`btmh` magnets, durable session and job state, storage safety,
DHT/uTP policy, qBittorrent/Transmission/Deluge facades, migration importers,
metrics, and diagnostics.

Current hardening focus is certification evidence on target hardware and live
client environments.

Pure-v2 `btmh` magnets are supported through the native bounded BEP 9/BEP 52
metadata path: the exact info dictionary and required piece layers are
authenticated before promotion. See
[`docs/API.md`](../../docs/API.md#pure-v2-boundary) for limits, restart
behavior, and the remaining qualification boundary.
