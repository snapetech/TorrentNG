use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum UtpError {
    #[error("header too short: {0} bytes")]
    HeaderTooShort(usize),

    #[error("unsupported uTP version: {0}")]
    UnsupportedVersion(u8),

    #[error("unknown packet type: {0}")]
    UnknownPacketType(u8),

    #[error(
        "uTP extension header truncated after extension type {extension}: {remaining} bytes remain"
    )]
    ExtensionHeaderTruncated { extension: u8, remaining: usize },

    #[error("uTP extension {extension} length {length} exceeds remaining payload {remaining}")]
    ExtensionTruncated {
        extension: u8,
        length: usize,
        remaining: usize,
    },

    #[error("uTP extension {extension} is too long to encode: {length} bytes")]
    ExtensionTooLong { extension: u8, length: usize },
}
