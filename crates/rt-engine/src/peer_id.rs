/// Our static peer ID: `-RN0100-` prefix (rusttorrentNG 0.1.0) + 12 zero bytes.
/// In production this should be randomised per session; zeros are fine for now.
pub const OUR_PEER_ID: [u8; 20] = *b"-RN0100-000000000000";
