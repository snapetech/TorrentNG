pub mod error;
pub mod magnet;
pub mod parse;
pub mod types;

pub use error::MetainfoError;
pub use magnet::parse_magnet;
pub use parse::{parse_torrent, torrent_info_bytes};
pub use types::{
    MagnetLink, TorrentFileV1, TorrentFileV2, TorrentMeta, TorrentMetaV1, TorrentMetaV2,
};
