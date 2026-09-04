# rTorrent XML-RPC library boundary

`rt-api-rtorrent` is a library facade, not a daemon and not an independently
deployable HTTP server. The public entry points are:

```rust
use rt_api_rtorrent::{execute_xml_with_token, AppState};

let response = execute_xml_with_token(&state, xml_request, Some(token)).await;
```

`AppState::with_engine` attaches a live TorrentNG engine. `with_tokens` enables
the library-boundary credential check; configured states must use
`execute_xml_with_token`. The no-token `execute_xml` helper is reserved for
local embedding and states with no configured credentials.

The native `torrentngd` process does not mount this facade as an XML-RPC HTTP
route. That is intentional: exposing the library helper directly would omit a
server-owned bind address, authentication middleware, request/body limits,
connection limits, timeout policy, and shutdown ownership. A consumer that
needs rTorrent XML-RPC over HTTP must supply a separate adapter that enforces
those controls, or use the existing Track 1 sidecar deployment.

The public library contract is tested as an external crate consumer in
`crates/rt-api-rtorrent/tests/library_entry_point.rs`, including credential
enforcement. This closes the documentation/test ambiguity without claiming an
HTTP deployment surface that does not exist.

The facade also rejects path-based loads at the library boundary. Embedded raw
metainfo and magnets are supported. Pure-v2 peer transfer and tracker lifecycle
remain explicit unsupported operations; see [API.md](API.md#pure-v2-boundary).
