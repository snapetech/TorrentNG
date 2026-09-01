#![no_main]

//! Fuzzes `rt_bencode::decode`, the lowest-level parser every bencoded
//! input in this codebase goes through -- `.torrent` files (via
//! rt-metainfo), HTTP tracker announce/scrape responses, and DHT KRPC
//! messages. All three are attacker-reachable: a hostile or compromised
//! tracker, a malicious DHT peer, or a crafted `.torrent` file can all put
//! arbitrary bytes in front of this function. Only checks for panics /
//! crashes -- an `Err` result for malformed input is correct.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rt_bencode::decode(data);
});
