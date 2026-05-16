// rt-bencode: canonical bencode parser and encoder
// Phase 0 stub — public API surface defined, implementation in Phase 1

pub mod decode;
pub mod encode;
pub mod error;

pub use decode::{BValue, Decoder};
pub use encode::encode;
pub use error::BencodeError;
