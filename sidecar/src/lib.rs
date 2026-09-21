pub mod api;
pub mod auth;
pub mod backend;
pub mod cache;
pub mod config;
pub mod identity;
mod log_sanitization;
pub mod media_type;
pub mod metrics;
mod multipart;
pub mod qbcompat;
pub mod rtorrent;
pub mod rtorrent_logs;
mod safe_file;
pub mod stats;
pub mod sync;
pub mod torrent_meta;
mod url_redaction;

/// Summarize a Tokio task failure without formatting its panic payload.
pub(crate) fn task_join_error_summary(task: &str, error: &tokio::task::JoinError) -> String {
    let outcome = if error.is_panic() {
        "panicked"
    } else {
        "was cancelled"
    };
    format!("{task} {outcome}")
}

/// Sanitize dynamically formatted text before writing it to operator logs.
pub use url_redaction::redact_display as redact_log_display;
