#![no_main]

//! Fuzzes `rt_peer_wire::Handshake::parse`, the first 68 bytes read from
//! every inbound and outbound peer-wire connection before anything else is
//! trusted. Every peer this daemon ever connects to or accepts a connection
//! from -- found via tracker, DHT, PEX, or a manually added peer -- sends
//! this handshake, so it is attacker-controlled input from the first byte.
//! Only checks for panics / crashes -- an `Err` for a malformed handshake is
//! correct and expected.

use libfuzzer_sys::fuzz_target;
use rt_peer_wire::{Handshake, HANDSHAKE_LEN};

fuzz_target!(|data: &[u8]| {
    if data.len() < HANDSHAKE_LEN {
        return;
    }
    let mut buf = [0u8; HANDSHAKE_LEN];
    buf.copy_from_slice(&data[..HANDSHAKE_LEN]);
    let _ = Handshake::parse(&buf);
});
