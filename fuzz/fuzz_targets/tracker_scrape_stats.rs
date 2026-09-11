#![no_main]

//! Fuzzes `rt_tracker::ScrapeStats::parse`, the bencoded scrape-response
//! decoder keyed by info-hash. Any tracker configured for a torrent can
//! send an arbitrary scrape reply here. The first 20 bytes of the fuzz
//! input stand in for the info-hash argument (padded with the whole input
//! if shorter) and the remainder is the bencoded body -- the split point is
//! arbitrary and not meant to model a real info-hash, only to vary both
//! parameters. Only checks for panics / crashes -- an `Err` for malformed
//! input is correct.

use libfuzzer_sys::fuzz_target;
use rt_tracker::ScrapeStats;

fuzz_target!(|data: &[u8]| {
    let (info_hash, bytes) = if data.len() > 20 {
        data.split_at(20)
    } else {
        (data, &[][..])
    };
    let _ = ScrapeStats::parse(bytes, info_hash);
});
