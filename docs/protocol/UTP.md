# BEP 29 uTP Status

TorrentNG now has a real `rt-utp` protocol crate, but the native engine does
not yet advertise full app-level uTP peer transport. The distinction matters:
the crate can open and exchange uTP packets over UDP, while the torrent engine
still routes peer-wire sessions through TCP.

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
- Process-level uTP counters exported through native Prometheus metrics:
  connects, accepts, sent/received bytes, send/receive timeouts,
  retransmissions, and route drops.
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

## Why `/health` Still Reports `utp_transport=false`

The runtime capability is intentionally stricter than crate capability. It must
mean peers can actually transfer torrent data through uTP in the native engine,
not merely that the protocol crate can exchange UDP packets.

Before flipping `networking.utp_transport=true`, the engine still needs:

- default peer dialing policy that chooses TCP or uTP based on
  tracker/DHT/PEX peer source, user preference, and private-torrent policy;
- metadata exchange over `UtpStream`;
- metrics for uTP connects, accepts, retransmits, timeouts, congestion window,
  RTT, bytes, and failures;
- integration tests proving a torrent can complete through uTP.

Until those pieces are wired, `networking.utp_packet_codec=true` and
`networking.utp_transport=false` is the honest app-level status.

The native health capability surface also reports:

- `networking.utp_udp_stream=true`: `rt-utp` has async UDP stream primitives.
- `networking.utp_outgoing_opt_in=true`: the engine contains an explicit
  outbound uTP peer-wire path.
- `networking.utp_incoming_opt_in=true`: the engine can bind a shared incoming
  uTP peer endpoint when `TNG_UTP_INCOMING=1` is set.
- `networking.utp_outgoing_policy`: `TNG_UTP_OUTGOING` when set, otherwise
  `prefer` for the legacy `TNG_ENABLE_UTP_OUTGOING` flag or `off`.
- `networking.utp_outgoing_enabled`: whether either outbound uTP opt-in is set.
- `networking.utp_incoming_enabled`: whether `TNG_UTP_INCOMING` is currently
  enabled for the engine process.

## Engine Integration Progress

The engine peer loop now has a transport-neutral `PeerIo` adapter and an
outbound `UtpStream` peer-wire path. `TNG_UTP_OUTGOING=prefer` attempts uTP and
falls back to TCP if the uTP connect fails; `TNG_UTP_OUTGOING=only` disables
that fallback. The legacy `TNG_ENABLE_UTP_OUTGOING` flag maps to `prefer`.
The engine also has an incoming shared `UtpEndpoint` peer listener behind
`TNG_UTP_INCOMING=1`; accepted streams are routed by peer-wire handshake to the
same torrent tasks as TCP peers. Magnet metadata fetch can use uTP through the
same byte-stream adapter when `TNG_UTP_METADATA=prefer|only` is set; if that is
not set it follows `TNG_UTP_OUTGOING`, then the legacy
`TNG_ENABLE_UTP_OUTGOING` flag.

The path remains opt-in while default policy, production interop, richer RTT and
congestion-window histograms, and tracker/DHT peer transport preference evidence
are still incomplete.
