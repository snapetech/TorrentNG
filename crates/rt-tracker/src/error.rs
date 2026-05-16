use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum TrackerError {
    #[error("tracker returned failure: {0}")]
    FailureReason(String),
    #[error("HTTP error: {status}")]
    Http { status: u16 },
    #[error("network error: {0}")]
    Network(String),
    #[error("UDP protocol error: {0}")]
    Udp(String),
    #[error("response parse error: {0}")]
    ParseError(String),
    #[error("invalid tracker URL: {0}")]
    InvalidUrl(String),
    #[error("announce timed out")]
    Timeout,
    #[error("tracker disabled")]
    Disabled,
    #[error("too many retries")]
    TooManyRetries,
}
