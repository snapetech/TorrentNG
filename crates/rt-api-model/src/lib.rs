pub mod auth;
pub mod error;
pub mod idempotency;
pub mod metrics;
pub mod snapshot;
pub mod torrent;

pub use auth::{csrf_request_allowed, has_session_cookie, session_cookie_value};
pub use error::ApiError;
pub use idempotency::{
    request_fingerprint, valid_idempotency_key, CachedResponse, Claim as IdempotencyClaim,
    IdempotencyExecutionGuard, IdempotencyStore, MAX_IDEMPOTENCY_BODY_BYTES,
    MAX_IDEMPOTENCY_KEY_BYTES,
};
pub use metrics::{ApiRuntimeMetrics, ApiRuntimeMetricsSnapshot, ApiSseClientGuard};
pub use snapshot::{ChunkedBitSet, ChunkedVec, SNAPSHOT_CHUNK_SIZE};
pub use torrent::{
    AddTorrentRequest, AddTorrentResponse, FileInfo, HashListRequest, TorrentDetail, TorrentSummary,
};
