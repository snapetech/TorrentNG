pub mod auth;
pub mod error;
pub mod idempotency;
pub mod limits;
pub mod metrics;
pub mod snapshot;
pub mod torrent;

pub use auth::{
    api_token_allowed, bearer_token, csrf_request_allowed, has_browser_request_headers,
    has_session_cookie, session_cookie_value, single_header_value, HeaderValueError,
};
pub use error::ApiError;
pub use idempotency::{
    cached_response_headers, is_replayable_response_header, request_fingerprint,
    valid_idempotency_key, CachedResponse, Claim as IdempotencyClaim, IdempotencyExecutionGuard,
    IdempotencyStore, MAX_IDEMPOTENCY_BODY_BYTES, MAX_IDEMPOTENCY_CACHE_BYTES,
    MAX_IDEMPOTENCY_KEY_BYTES,
};
pub use limits::{api_uri_is_bounded, MAX_API_URI_BYTES};
pub use metrics::{ApiRuntimeMetrics, ApiRuntimeMetricsSnapshot, ApiSseClientGuard};
pub use snapshot::{ChunkedBitSet, ChunkedVec, SNAPSHOT_CHUNK_SIZE};
pub use torrent::{
    AddTorrentRequest, AddTorrentResponse, FileInfo, HashListRequest, TorrentDetail, TorrentSummary,
};
