# rusttorrentd

Native Rust BitTorrent daemon. Wires all engine crates, handles signals, startup, shutdown.

## Status: Implemented — native daemon in active hardening

`rusttorrentd` runs the native Rust engine, native REST/SSE API, qBittorrent
compatibility facade, Transmission facade, peer listener, tracker manager,
DHT task, durable SQLite session state, and bounded clean shutdown. It does not
require the Phase 1 rTorrent sidecar or XMLRPC path.

`GET /health` exposes a native-engine capability manifest covering v1/v2/hybrid
identity, `btih`/`btmh` magnets, durable session and job state, storage safety,
DHT/uTP policy, qBittorrent/Transmission/Deluge facades, migration importers,
metrics, and diagnostics.

Current hardening focus is certification evidence on target hardware and live
client environments.
