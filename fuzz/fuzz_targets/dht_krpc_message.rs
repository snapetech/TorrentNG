#![no_main]

//! Fuzzes `rt_dht::KrpcMessage::parse`, the decoder for every UDP packet
//! this daemon's DHT node receives. Unlike peer-wire and tracker traffic,
//! DHT UDP packets arrive from nodes this daemon has never explicitly
//! connected to or vetted -- reachability is the entire point of a DHT --
//! so this parser sees the least-trusted input of any codec in the
//! codebase. Only checks for panics / crashes -- an `Err` for a malformed
//! packet is correct.

use libfuzzer_sys::fuzz_target;
use rt_dht::KrpcMessage;

fuzz_target!(|data: &[u8]| {
    let _ = KrpcMessage::parse(data);
});
