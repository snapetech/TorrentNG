#![no_main]

//! Fuzzes `rt_tracker::udp::UdpConnectResponse::parse` (BEP 15), the reply a
//! UDP tracker sends to establish a connection ID. Any tracker configured
//! for a torrent -- including one added via a `.torrent` file from an
//! untrusted source -- can send an arbitrary reply here. Only checks for
//! panics / crashes -- an `Err` for a malformed response is correct.

use libfuzzer_sys::fuzz_target;
use rt_tracker::udp::UdpConnectResponse;

fuzz_target!(|data: &[u8]| {
    let _ = UdpConnectResponse::parse(data);
});
