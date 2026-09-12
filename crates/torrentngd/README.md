# torrentngd

TorrentNG's first-party BitTorrent client daemon. Wires the client engine crates,
handles signals, startup, and shutdown.

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
