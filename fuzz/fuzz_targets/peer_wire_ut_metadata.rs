#![no_main]

//! Fuzzes `rt_peer_wire::UtMetadataMessage::parse`, the BEP 9 metadata
//! exchange message a peer sends when this daemon is fetching torrent
//! metadata from the swarm instead of a `.torrent` file. This path is
//! reachable from any peer claiming ut_metadata support in its extension
//! handshake, before the real metainfo (and its own hash-verified integrity
//! check) exists at all. Only checks for panics / crashes -- an `Err` for a
//! malformed message is correct.

use libfuzzer_sys::fuzz_target;
use rt_peer_wire::UtMetadataMessage;

fuzz_target!(|data: &[u8]| {
    let _ = UtMetadataMessage::parse(data);
});
