// rt-bencode: canonical bencode parser and encoder

pub mod decode;
pub mod encode;
pub mod error;

pub use decode::{decode, decode_torrent_info_span, BValue, Decoder};
pub use encode::encode;
pub use error::BencodeError;
