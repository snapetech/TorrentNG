# rt-utp

uTP packet and transport-state primitives for the native engine.

## Status: Packet codec and transport-state primitives implemented — socket integration pending

This crate currently provides BEP 29 fixed-header parsing/encoding, full packet
payload framing, extension-chain parsing/encoding with truncation and
oversized-extension validation, selective ACK helpers, connection ID derivation,
state transitions, send-window accounting, RTT sampling, retransmit timeout
tracking, delay-based congestion-window adjustment, and an async UDP
`UtpListener`/`UtpStream` transport with SYN/STATE handshake, DATA/ACK exchange,
FIN close, and bounded retransmission attempts.

Full native-engine uTP integration remains a hardening item: the remaining work
is wiring `UtpStream` into peer-wire handshakes, peer selection, incoming peer
dispatch, and engine lifecycle. The native `/health` capability manifest reports
this split explicitly as `networking.utp_packet_codec=true` and
`networking.utp_transport=false` until that end-to-end engine path is active.

See `docs/protocol/UTP.md` for the module inventory, tested behavior, public API
example, and engine-integration checklist.
