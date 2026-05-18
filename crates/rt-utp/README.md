# rt-utp

uTP packet and transport-state primitives for the native engine.

## Status: Packet codec and transport-state primitives implemented — socket integration pending

This crate currently provides BEP 29 fixed-header parsing/encoding, full packet
payload framing, extension-chain parsing/encoding with truncation and
oversized-extension validation, selective ACK helpers, connection ID derivation,
state transitions, send-window accounting, RTT sampling, and retransmit timeout
tracking.

Full uTP socket transport integration remains a native-engine hardening item:
the remaining work is the async UDP driver that wires these primitives to
peer-wire handshakes, retransmission scheduling, and engine peer lifecycle. The
native `/health` capability manifest reports this split explicitly as
`networking.utp_packet_codec=true` and `networking.utp_transport=false`.
