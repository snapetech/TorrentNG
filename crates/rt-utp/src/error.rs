use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum UtpError {
    #[error("header too short: {0} bytes")]
    HeaderTooShort(usize),

    #[error("unsupported uTP version: {0}")]
    UnsupportedVersion(u8),

    #[error("unknown packet type: {0}")]
    UnknownPacketType(u8),
}
