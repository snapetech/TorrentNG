use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum WireError {
    #[error("handshake protocol mismatch: expected BitTorrent protocol")]
    ProtocolMismatch,

    #[error("infohash mismatch in handshake")]
    InfohashMismatch,

    #[error(
        "message length {0} exceeds maximum {}",
        crate::message::MAX_MESSAGE_LEN
    )]
    MessageTooLarge(u32),

    #[error("unknown message id: {0}")]
    UnknownMessageId(u8),

    #[error("invalid message: {0}")]
    InvalidMessage(String),

    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        WireError::Io(e.to_string())
    }
}
