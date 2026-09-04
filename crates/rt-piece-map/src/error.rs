use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum PieceMapError {
    #[error("piece index {0} out of range (count: {1})")]
    PieceOutOfRange(u32, u32),
    #[error("request exceeds max block size: {0} > {1}")]
    BlockTooLarge(u32, u32),
    #[error("request length must be greater than zero")]
    ZeroRequestLength,
    #[error(
        "request offset + length overflows piece: offset={offset} len={len} piece_len={piece_len}"
    )]
    RequestOutOfBounds {
        offset: u32,
        len: u32,
        piece_len: u32,
    },
    #[error("zero piece length")]
    ZeroPieceLength,
    #[error("zero total length")]
    ZeroTotalLength,
    #[error("piece length exceeds supported u32 range: {0}")]
    PieceLengthTooLarge(u64),
    #[error("piece count exceeds supported u32 range: {0}")]
    PieceCountTooLarge(u64),
    #[error("file span {file_index} starts at {actual}, expected {expected}")]
    NonContiguousFileSpan {
        file_index: u32,
        expected: u64,
        actual: u64,
    },
    #[error("integer overflow while building piece map: {0}")]
    IntegerOverflow(&'static str),
}
