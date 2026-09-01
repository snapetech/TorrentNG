use thiserror::Error;

use rt_path::PathError;

#[derive(Debug, Error)]
pub enum MetainfoError {
    #[error("bencode error: {0}")]
    Bencode(#[from] rt_bencode::BencodeError),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid field type for: {0}")]
    InvalidFieldType(&'static str),
    #[error("invalid integer value for {field}: {value}")]
    InvalidIntegerValue { field: &'static str, value: i64 },
    #[error("integer overflow while parsing field: {0}")]
    IntegerOverflow(&'static str),
    #[error("metainfo limit exceeded for {field}: limit {limit}")]
    LimitExceeded { field: &'static str, limit: usize },
    #[error("invalid pieces length: {0} (must be multiple of 20)")]
    InvalidPiecesLength(usize),
    #[error("invalid piece count: expected {expected}, got {actual}")]
    InvalidPieceCount { expected: usize, actual: usize },
    #[error("invalid piece length: {0}")]
    InvalidPieceLength(u64),
    #[error("invalid file path: {0}")]
    InvalidPath(#[from] PathError),
    #[error("invalid UTF-8 in field: {0}")]
    InvalidUtf8(&'static str),
    #[error("zero-length name")]
    ZeroLengthName,
    #[error("announce URL invalid: {0}")]
    InvalidAnnounce(String),
    #[error("invalid magnet link: {0}")]
    InvalidMagnet(String),
    #[error("unsupported magnet info hash: {0}")]
    UnsupportedMagnetHash(String),
}
