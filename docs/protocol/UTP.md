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

`UtpTransportConfig` exposes handshake timeout, I/O timeout, max datagram size,
and retransmission-attempt bounds.

## Why `/health` Still Reports `utp_transport=false`

The runtime capability is intentionally stricter than crate capability. It must
mean peers can actually transfer torrent data through uTP in the native engine,
not merely that the protocol crate can exchange UDP packets.

Before flipping `networking.utp_transport=true`, the engine still needs:

- incoming UDP listener ownership next to the TCP listener;
- demultiplexing by connection ID and remote address;
- outbound peer dialing that chooses TCP or uTP based on tracker/DHT/PEX peer
  source, user preference, and private-torrent policy;
- peer-wire handshake over `UtpStream`;
- metadata exchange over `UtpStream`;
- piece request/response loops over `UtpStream`;
- per-peer lifecycle accounting, caps, scoring, and ban/eject behavior shared
  with TCP peers;
- metrics for uTP connects, accepts, retransmits, timeouts, congestion window,
  RTT, bytes, and failures;
- integration tests proving a torrent can complete through uTP.

Until those pieces are wired, `networking.utp_packet_codec=true` and
`networking.utp_transport=false` is the honest app-level status.
