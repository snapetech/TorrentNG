use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum JobError {
    #[error("job {0} not found")]
    NotFound(String),
    #[error("job {0} cannot be cancelled in state {1}")]
    NotCancellable(String, String),
    #[error("job {0} is already in terminal state {1}")]
    AlreadyTerminal(String, String),
    #[error("dry-run required for destructive job kind: {0}")]
    DryRunRequired(String),
}
