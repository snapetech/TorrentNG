# BEP 29 uTP Status

TorrentNG now has a real `rt-utp` protocol crate, but the native engine does
not treat the packet codec alone as full application support. The distinction
matters: the crate can open and exchange uTP packets over UDP, while the native
engine capability only reports full transport when torrent peer-wire or
metadata paths can actually use those streams.

## Implemented In `rt-utp`

- BEP 29 fixed header parsing and encoding for SYN, STATE, DATA, FIN, and RESET.
- Packet payload framing and extension-chain parsing/encoding.
- Selective ACK extension bitset helpers.
- Initiator and acceptor connection ID derivation.
- Connection state transitions for SYN, STATE, DATA, FIN, and RESET.
- ACK validation, in-flight byte accounting, advertised-window handling, and
  RTT/retransmit-timeout sampling.
- Delay-based congestion-window adjustment with a minimum MTU floor and timeout
  backoff.
- Async UDP `UtpListener` and `UtpStream` primitives:
  - listener bind and accept;
  - client connect;
  - SYN/STATE handshake;
  - DATA send and ACK wait;
  - payload receive and ACK response;
  - FIN close;
  - bounded retransmission attempts.
- Shared UDP `UtpEndpoint` acceptor:
  - binds one UDP socket for the peer port;
  - demultiplexes packets by remote address and receive connection ID;
  - accepts multiple incoming uTP streams without consuming the listener socket;
  - routes DATA/FIN/STATE packets into per-stream bounded queues.
- Process-level uTP counters and gauges exported through native Prometheus
  metrics: connects, accepts, sent/received bytes, send/receive timeouts,
  retransmissions, route drops, RTT, RTT variance, retransmit timeout,
  congestion window, delay samples, and bytes in flight.
- Byte-stream helpers over DATA payloads:
  - `write_all` chunks arbitrary byte slices across uTP DATA packets;
  - `read_exact` buffers received DATA payloads and can satisfy reads across
    packet boundaries, which is the bridge needed for peer-wire handshakes and
    length-prefixed messages.

## Tested Behavior

The local `rt-utp` test suite covers:

- header and packet roundtrips;
- malformed header, version, type, extension header, extension payload, and
  oversized extension errors;
- selective ACK bitset generation and decoding;
- initiator/acceptor connection ID rules;
- SYN-to-STATE establishment;
- DATA sequence advancement and receive ACK updates;
- FIN/RESET state handling through the connection state machine;
- ACK out-of-window rejection and zero-ACK startup handling;
- congestion-window growth, reduction, and timeout floor behavior;
- loopback UDP transport handshake, payload exchange, ACK, and close.
- byte-stream reads spanning multiple uTP payload chunks.

Run:

```sh
cargo test -p rt-utp
```

## Public API

Minimal loopback-style usage:

```rust
use rt_utp::{UtpListener, UtpStream};

let listener = UtpListener::bind("127.0.0.1:0".parse().unwrap()).await?;
let addr = listener.local_addr()?;

let server = tokio::spawn(async move {
    let mut stream = listener.accept().await?;
    let payload = stream.recv().await?;
    stream.send(&payload).await?;
    stream.close().await
});

let mut client = UtpStream::connect(addr).await?;
client.send(b"piece block").await?;
let echoed = client.recv().await?;
client.close().await?;
server.await??;
```

For peer-wire integration, callers can use the byte-stream helpers:

```rust
let mut hs = [0u8; 68];
stream.write_all(&handshake_bytes).await?;
stream.read_exact(&mut hs).await?;
```

`UtpTransportConfig` exposes handshake timeout, I/O timeout, max datagram size,
and retransmission-attempt bounds.

## `/health` uTP Transport Capability

The runtime capability is intentionally stricter than crate capability. It
means peers can transfer torrent data or metadata through uTP in the native
engine, not merely that the protocol crate can exchange UDP packets.

`/health` now reports `networking.utp_transport=true` whenever at least one
runtime path can use uTP:

- outbound peer-wire is enabled by `TNG_UTP_OUTGOING` or by the default `auto`
  policy;
- metadata fetch is enabled by `TNG_UTP_METADATA` or the legacy outgoing flag;
- incoming uTP accepts are enabled by `TNG_UTP_INCOMING=1`.

If operators force TCP-only mode with `TNG_UTP_OUTGOING=tcp-only`, leave
metadata uTP off, and do not enable incoming uTP, `/health` reports
`networking.utp_transport=false`.

The native health capability surface also reports:

- `networking.utp_udp_stream=true`: `rt-utp` has async UDP stream primitives.
- `networking.utp_outgoing_opt_in=true`: the engine contains an explicit
  outbound uTP peer-wire path.
- `networking.utp_incoming_opt_in=true`: the engine can bind a shared incoming
  uTP peer endpoint when `TNG_UTP_INCOMING=1` is set.
- `networking.utp_outgoing_policy`: `TNG_UTP_OUTGOING` when set, otherwise
  `prefer` for the legacy `TNG_ENABLE_UTP_OUTGOING` flag or `auto`.
- `networking.utp_outgoing_enabled`: whether outbound uTP may be attempted for
  at least one peer source under the current policy.
- `networking.utp_metadata_policy`: `TNG_UTP_METADATA` when set, otherwise
  `TNG_UTP_OUTGOING`, legacy `prefer`, or `off`.
- `networking.utp_metadata_enabled`: whether metadata fetch can attempt uTP.
- `networking.utp_incoming_enabled`: whether `TNG_UTP_INCOMING` is currently
  enabled for the engine process. This is a boolean listener switch; use
  `1`, `true`, `yes`, or `on`.
- `networking.utp_transport_paths`: the runtime paths that can currently use
  uTP, drawn from `outgoing_peer_wire`, `metadata_fetch`, and
  `incoming_peer_wire`.

## Engine Integration Progress

The engine peer loop now has a transport-neutral `PeerIo` adapter and an
outbound `UtpStream` peer-wire path. The default `TNG_UTP_OUTGOING=auto`
policy keeps tracker-discovered peers on TCP, attempts uTP first for DHT, PEX,
and manually added peers, and forces TCP for private torrents. Explicit
`TNG_UTP_OUTGOING=prefer` attempts uTP for eligible peers and falls back to TCP
if the uTP connect fails; `TNG_UTP_OUTGOING=only` disables that fallback. The
legacy `TNG_ENABLE_UTP_OUTGOING` flag maps to `prefer`.
The engine also has an incoming shared `UtpEndpoint` peer listener behind
`TNG_UTP_INCOMING=1`; accepted streams are routed by peer-wire handshake to the
same torrent tasks as TCP peers. Magnet metadata fetch can use uTP through the
same byte-stream adapter when `TNG_UTP_METADATA=prefer|only` is set; if that is
not set it follows `TNG_UTP_OUTGOING`, then the legacy
`TNG_ENABLE_UTP_OUTGOING` flag.

Production interop, alert thresholds, dashboards, and tracker/DHT transport
preference evidence remain release-quality work, but they no longer hide the
implemented app-level uTP transport from the runtime capability surface.
