use serde::{Deserialize, Serialize};

/// Canonical API error response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        ApiError {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        ApiError::new("NOT_FOUND", msg)
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        ApiError::new("BAD_REQUEST", msg)
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        ApiError::new("INTERNAL_ERROR", msg)
    }

    pub fn not_implemented(msg: impl Into<String>) -> Self {
        ApiError::new("NOT_IMPLEMENTED", msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_serializes() {
        let e = ApiError::not_found("hash not found");
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("NOT_FOUND"));
        assert!(json.contains("hash not found"));
    }
}
