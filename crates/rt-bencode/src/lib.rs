// rt-bencode: canonical bencode parser and encoder

pub mod decode;
pub mod encode;
pub mod error;

pub use decode::{
    decode, decode_torrent_info_span, decode_torrent_info_span_with_allocation_reservation,
    decode_with_allocation_reservation, BValue, Decoder,
};
pub use encode::encode;
pub use error::BencodeError;
