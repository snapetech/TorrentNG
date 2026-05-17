# rt-utp

uTP packet primitives for the native engine.

## Status: Packet layer implemented — transport integration pending

This crate currently provides uTP header and packet parsing/encoding building
blocks. Full socket transport integration remains a native-engine hardening
item; the `/health` capability manifest reports runtime policy separately from
this packet-layer crate.
