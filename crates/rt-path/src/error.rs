use thiserror::Error;

#[derive(Debug, Error, PartialEq, Clone)]
pub enum PathError {
    #[error("absolute path not allowed: {0}")]
    AbsolutePath(String),
    #[error("parent traversal not allowed: {0}")]
    ParentTraversal(String),
    #[error("empty path component")]
    EmptyComponent,
    #[error("NUL byte in path component")]
    NulByte,
    #[error("Windows reserved name: {0}")]
    WindowsReservedName(String),
    #[error("path component contains illegal character: {0:?}")]
    IllegalCharacter(char),
    #[error("path is empty")]
    EmptyPath,
}
