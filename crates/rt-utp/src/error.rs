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

    #[error("uTP packet type {packet_type:?} is invalid while connection is {state:?}")]
    InvalidStatePacket {
        state: crate::state::ConnectionState,
        packet_type: crate::packet::PacketType,
    },

    #[error("uTP packet connection id {actual} did not match expected {expected}")]
    ConnectionIdMismatch { expected: u16, actual: u16 },

    #[error("uTP ack {ack_nr} is outside send window {oldest_unacked}..={newest_sent}")]
    AckOutOfWindow {
        ack_nr: u16,
        oldest_unacked: u16,
        newest_sent: u16,
    },

    #[error("uTP I/O error: {0}")]
    Io(String),

    #[error("uTP operation timed out")]
    Timeout,

    #[error("uTP stream closed")]
    Closed,
}
