use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Cross-facade counters for the bounded snapshot and SSE paths. Native and
/// qBittorrent routers can share one instance in the daemon, so the metrics
/// endpoint reports the combined API pressure instead of whichever facade
/// happened to be constructed first.
#[derive(Debug, Default)]
pub struct ApiRuntimeMetrics {
    snapshot_refreshes_total: AtomicU64,
    snapshot_incremental_updates_total: AtomicU64,
    snapshot_expired_total: AtomicU64,
    sse_resyncs_total: AtomicU64,
    sse_events_total: AtomicU64,
    sse_lagged_total: AtomicU64,
    sse_disconnects_total: AtomicU64,
    sse_clients: AtomicU64,
    response_bytes_estimated_total: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApiRuntimeMetricsSnapshot {
    pub snapshot_refreshes_total: u64,
    pub snapshot_incremental_updates_total: u64,
    pub snapshot_expired_total: u64,
    pub sse_resyncs_total: u64,
    pub sse_events_total: u64,
    pub sse_lagged_total: u64,
    pub sse_disconnects_total: u64,
    pub sse_clients: u64,
    pub response_bytes_estimated_total: u64,
}

impl ApiRuntimeMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_snapshot_refresh(&self) {
        self.snapshot_refreshes_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_snapshot_incremental_update(&self) {
        self.snapshot_incremental_updates_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_snapshot_expired(&self) {
        self.snapshot_expired_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_sse_resync(&self) {
        self.sse_resyncs_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_sse_event(&self) {
        self.sse_events_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_sse_lagged(&self) {
        self.sse_lagged_total.fetch_add(1, Ordering::Relaxed);
    }

    fn record_sse_disconnect(&self) {
        self.sse_disconnects_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_estimated_response_bytes(&self, bytes: u64) {
        self.response_bytes_estimated_total
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn register_sse_client(self: &Arc<Self>) -> ApiSseClientGuard {
        self.sse_clients.fetch_add(1, Ordering::Relaxed);
        ApiSseClientGuard {
            metrics: Arc::clone(self),
        }
    }

    pub fn snapshot(&self) -> ApiRuntimeMetricsSnapshot {
        ApiRuntimeMetricsSnapshot {
            snapshot_refreshes_total: self.snapshot_refreshes_total.load(Ordering::Relaxed),
            snapshot_incremental_updates_total: self
                .snapshot_incremental_updates_total
                .load(Ordering::Relaxed),
            snapshot_expired_total: self.snapshot_expired_total.load(Ordering::Relaxed),
            sse_resyncs_total: self.sse_resyncs_total.load(Ordering::Relaxed),
            sse_events_total: self.sse_events_total.load(Ordering::Relaxed),
            sse_lagged_total: self.sse_lagged_total.load(Ordering::Relaxed),
            sse_disconnects_total: self.sse_disconnects_total.load(Ordering::Relaxed),
            sse_clients: self.sse_clients.load(Ordering::Relaxed),
            response_bytes_estimated_total: self
                .response_bytes_estimated_total
                .load(Ordering::Relaxed),
        }
    }
}

/// Decrements the active SSE-client gauge when the stream state is dropped.
#[derive(Debug)]
pub struct ApiSseClientGuard {
    metrics: Arc<ApiRuntimeMetrics>,
}

impl Drop for ApiSseClientGuard {
    fn drop(&mut self) {
        self.metrics.record_sse_disconnect();
        let mut current = self.metrics.sse_clients.load(Ordering::Relaxed);
        while current > 0 {
            match self.metrics.sse_clients.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_metrics_track_snapshot_and_sse_lifecycle() {
        let metrics = ApiRuntimeMetrics::new();
        metrics.record_snapshot_refresh();
        metrics.record_snapshot_incremental_update();
        metrics.record_snapshot_expired();
        metrics.record_sse_resync();
        metrics.record_sse_event();
        metrics.record_sse_lagged();
        metrics.record_estimated_response_bytes(42);
        let client = metrics.register_sse_client();
        assert_eq!(metrics.snapshot().sse_clients, 1);
        assert_eq!(metrics.snapshot().response_bytes_estimated_total, 42);
        drop(client);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.snapshot_refreshes_total, 1);
        assert_eq!(snapshot.snapshot_incremental_updates_total, 1);
        assert_eq!(snapshot.snapshot_expired_total, 1);
        assert_eq!(snapshot.sse_resyncs_total, 1);
        assert_eq!(snapshot.sse_events_total, 1);
        assert_eq!(snapshot.sse_lagged_total, 1);
        assert_eq!(snapshot.sse_disconnects_total, 1);
        assert_eq!(snapshot.sse_clients, 0);
    }
}
