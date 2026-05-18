# rt-utp

uTP packet and transport-state primitives for the native engine.

## Status: Packet codec, UDP stream, and opt-in engine paths implemented

This crate currently provides BEP 29 fixed-header parsing/encoding, full packet
payload framing, extension-chain parsing/encoding with truncation and
oversized-extension validation, selective ACK helpers, connection ID derivation,
state transitions, send-window accounting, RTT sampling, retransmit timeout
tracking, delay-based congestion-window adjustment, and an async UDP
`UtpListener`/`UtpStream` transport with SYN/STATE handshake, DATA/ACK exchange,
FIN close, bounded retransmission attempts, a shared-socket `UtpEndpoint`
incoming acceptor with connection-ID demux, and byte-stream `read_exact` /
`write_all` helpers over uTP DATA payloads.

Native-engine uTP integration is partially active: outbound peer-wire sessions
can use `UtpStream` when enabled by `TNG_UTP_OUTGOING=prefer|only` or the legacy
`TNG_ENABLE_UTP_OUTGOING` flag. Prefer mode attempts uTP first and falls back to
TCP if the uTP connect fails. Incoming peer-wire sessions can use `UtpEndpoint`
when enabled by `TNG_UTP_INCOMING=1`. Default transport policy and end-to-end
torrent completion certification remain hardening items, so the native
`/health` manifest still reports `networking.utp_transport=false` until the full
incoming/outgoing app-level path is active by default.

See `docs/protocol/UTP.md` for the module inventory, tested behavior, public API
example, and engine-integration checklist.
