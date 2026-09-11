#![no_main]

//! Fuzzes `rt_tracker::udp::UdpAnnounceResponse::parse` (BEP 15), the reply
//! a UDP tracker sends to an announce -- including the compact peer list
//! this daemon will then try to connect to. Any tracker configured for a
//! torrent can send an arbitrary reply here. Only checks for panics /
//! crashes -- an `Err` for a malformed response is correct.

use libfuzzer_sys::fuzz_target;
use rt_tracker::udp::UdpAnnounceResponse;

fuzz_target!(|data: &[u8]| {
    let _ = UdpAnnounceResponse::parse(data);
});
