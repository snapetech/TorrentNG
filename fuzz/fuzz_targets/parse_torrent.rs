#![no_main]

//! Fuzzes `rt_metainfo::parse_torrent`, the entry point for every `.torrent`
//! file this daemon ever reads -- torrents added via the API, migrated from
//! another client, or restored from persisted state on disk. This data is
//! attacker-controlled in the ordinary sense that any `.torrent` file from
//! any source (a public tracker, a friend, a scraped index) reaches this
//! parser before anything else touches it. The only property this target
//! checks is "never panics" -- `parse_torrent` returning `Err` for malformed
//! input is correct and expected; a panic, out-of-bounds read, or unbounded
//! allocation is not.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rt_metainfo::parse_torrent(data);
});
