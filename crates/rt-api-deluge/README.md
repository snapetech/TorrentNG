# rt-api-deluge

Deluge JSON-RPC compatibility facade over native engine state.

## Status: Implemented — best-effort compatibility facade

This crate exposes `/json` and `/deluge/json` for clients that expect Deluge
method names. It is intentionally a compatibility projection over the native
registry, not a Deluge-compatible internal model.

Supported surfaces include auth/session probes, host status, torrent list/detail
projection, file and tracker views, pause/resume/recheck/remove, add torrent
file, add magnet, storage moves, and selected WebUI update calls. Unsupported
plugin-management and UI-only methods degrade to compatible no-op responses
where safe.

Run focused tests:

```sh
cargo test -p rt-api-deluge
```
