#![no_main]

//! Fuzzes `rt_peer_wire::Message::parse`, the steady-state peer-wire message
//! decoder used for every message after the handshake -- have, bitfield,
//! request, piece, cancel, extended, and every other message type a
//! connected peer can send for the lifetime of the connection. This is the
//! single highest-volume attacker-reachable parser in the daemon: every
//! byte a peer sends after the handshake passes through it. Only checks for
//! panics / crashes -- an `Err` for a malformed message is correct.

use libfuzzer_sys::fuzz_target;
use rt_peer_wire::Message;

fuzz_target!(|data: &[u8]| {
    let _ = Message::parse(data);
});
