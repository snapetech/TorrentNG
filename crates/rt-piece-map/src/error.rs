use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum PieceMapError {
    #[error("piece index {0} out of range (count: {1})")]
    PieceOutOfRange(u32, u32),
    #[error("request exceeds max block size: {0} > {1}")]
    BlockTooLarge(u32, u32),
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
}
