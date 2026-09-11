#![no_main]

//! Fuzzes `rt_tracker::AnnounceResponse::parse`, the bencoded-dictionary
//! decoder for an HTTP tracker's announce reply, including the peer list
//! this daemon will then try to connect to. Any tracker configured for a
//! torrent -- including one whose URL came from a `.torrent` file of
//! unknown provenance -- can send an arbitrary reply here. Only checks for
//! panics / crashes -- an `Err` for a malformed response is correct.

use libfuzzer_sys::fuzz_target;
use rt_tracker::AnnounceResponse;

fuzz_target!(|data: &[u8]| {
    let _ = AnnounceResponse::parse(data);
});
