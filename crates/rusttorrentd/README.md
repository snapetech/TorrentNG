# rusttorrentd

Native Rust BitTorrent daemon. Wires all engine crates, handles signals, startup, shutdown.

## Status: Implemented — native daemon in active hardening

`rusttorrentd` runs the native Rust engine, native REST/SSE API, qBittorrent
compatibility facade, Transmission facade, peer listener, tracker manager,
DHT task, durable SQLite session state, and bounded clean shutdown. It does not
require the Phase 1 rTorrent sidecar or XMLRPC path.

Current hardening focus is certification and migration coverage: large-library
dry-run import, public download certification, compatibility certification, and
legacy client resume-state import.
