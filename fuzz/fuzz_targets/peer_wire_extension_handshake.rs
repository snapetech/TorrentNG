#![no_main]

//! Fuzzes `rt_peer_wire::ExtensionHandshake::parse`, the BEP 10 extension
//! handshake payload a peer sends to advertise which extended messages
//! (ut_metadata, PEX, ...) it supports. Any peer that claims extension
//! protocol support sends this, unauthenticated, before any extended
//! message can be trusted. Only checks for panics / crashes -- an `Err` for
//! a malformed payload is correct.

use libfuzzer_sys::fuzz_target;
use rt_peer_wire::ExtensionHandshake;

fuzz_target!(|data: &[u8]| {
    let _ = ExtensionHandshake::parse(data);
});
