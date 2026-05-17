# rt-api-transmission

Transmission RPC compatibility facade over native engine state.

## Status: Implemented — compatibility facade

This crate exposes Transmission-style session and torrent methods for clients
that speak `/transmission/rpc` or `/api/transmission/rpc`. It projects the same
native registry used by the WebUI and qBittorrent facade.

Supported surfaces include session info, torrent list/detail, add/remove,
start/stop, verify, reannounce, tracker and file projection, queue fields, and
magnet links. BEP 52/v2 hashes project as `btmh` where applicable.

Run focused tests:

```sh
cargo test -p rt-api-transmission
```
