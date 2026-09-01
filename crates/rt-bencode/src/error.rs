use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum BencodeError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("invalid integer: {0}")]
    InvalidInteger(String),
    #[error("invalid string length: {0}")]
    InvalidStringLength(String),
    #[error("string too long: {len} > max {max}")]
    StringTooLong { len: usize, max: usize },
    #[error("recursion depth exceeded: {0}")]
    DepthExceeded(usize),
    #[error("node limit exceeded: {nodes} > max {max}")]
    NodeLimitExceeded { nodes: usize, max: usize },
    #[error("unsorted dictionary keys")]
    UnsortedDictKeys,
    #[error("unexpected byte {0:#04x} at offset {1}")]
    UnexpectedByte(u8, usize),
    #[error("trailing data after value")]
    TrailingData,
}
