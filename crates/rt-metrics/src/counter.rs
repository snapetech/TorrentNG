/// Lightweight atomic performance counters for upload/download accounting.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn reset(&self) -> u64 {
        self.0.swap(0, Ordering::Relaxed)
    }
}

/// Global runtime metrics snapshot.
#[derive(Debug, Default)]
pub struct Metrics {
    pub bytes_uploaded: Counter,
    pub bytes_downloaded: Counter,
    pub announce_count: Counter,
    pub peer_connections: Counter,
    pub piece_writes: Counter,
    pub piece_reads: Counter,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Metrics::default())
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            bytes_uploaded: self.bytes_uploaded.get(),
            bytes_downloaded: self.bytes_downloaded.get(),
            announce_count: self.announce_count.get(),
            peer_connections: self.peer_connections.get(),
            piece_writes: self.piece_writes.get(),
            piece_reads: self.piece_reads.get(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub announce_count: u64,
    pub peer_connections: u64,
    pub piece_writes: u64,
    pub piece_reads: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_inc_and_get() {
        let c = Counter::default();
        c.inc(100);
        c.inc(200);
        assert_eq!(c.get(), 300);
    }

    #[test]
    fn counter_reset() {
        let c = Counter::default();
        c.inc(500);
        let prev = c.reset();
        assert_eq!(prev, 500);
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn metrics_snapshot() {
        let m = Metrics::new();
        m.bytes_uploaded.inc(1_000_000);
        m.announce_count.inc(3);
        let snap = m.snapshot();
        assert_eq!(snap.bytes_uploaded, 1_000_000);
        assert_eq!(snap.announce_count, 3);
        assert_eq!(snap.bytes_downloaded, 0);
    }
}
