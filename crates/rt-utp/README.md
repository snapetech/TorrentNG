# rt-utp

uTP packet primitives for the native engine.

## Status: Packet codec implemented — transport integration pending

This crate currently provides BEP 29 fixed-header parsing/encoding, full packet
payload framing, and extension-chain parsing/encoding with truncation and
oversized-extension validation.

Full uTP socket transport integration remains a native-engine hardening item.
The native `/health` capability manifest reports this split explicitly as
`networking.utp_packet_codec=true` and `networking.utp_transport=false`.
