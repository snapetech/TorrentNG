pub mod error;
pub mod magnet;
pub mod parse;
pub mod types;

pub use error::MetainfoError;
pub use magnet::parse_magnet;
pub use parse::{
    parse_torrent, torrent_info_bytes, v2_piece_layer_requirements, MAX_TORRENT_BYTES,
};
pub use types::{
    MagnetLink, TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2,
    V2PieceLayerRequirement, V2PieceLayerRequirements,
};
