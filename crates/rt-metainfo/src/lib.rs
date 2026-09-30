pub mod error;
pub mod magnet;
pub mod parse;
pub mod types;

pub use error::MetainfoError;
pub use magnet::parse_magnet;
pub use parse::{
    parse_torrent, parse_torrent_with_allocation_reservation, torrent_info_bytes,
    torrent_info_bytes_with_allocation_reservation, v2_piece_layer_requirements,
    v2_piece_layer_requirements_with_allocation_reservation, validate_hybrid_file_layout,
    MAX_CONVENIENCE_METAINFO_ALLOCATION_BYTES, MAX_TORRENT_BYTES,
};
pub use types::{
    MagnetLink, TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2,
    V2PieceLayerRequirement, V2PieceLayerRequirements,
};
