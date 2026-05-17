use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

/// Global metric counters. Cheap atomics — no external crate needed for Phase 2.
/// Replace with prometheus crate when the metric set grows enough to warrant it.
#[derive(Default)]
pub struct Metrics {
    pub torrents_total: AtomicI64,
    pub torrents_seeding: AtomicI64,
    pub torrents_downloading: AtomicI64,
    pub torrents_stopped: AtomicI64,
    pub torrents_errored: AtomicI64,
    pub peers_connected: AtomicI64,
    pub api_requests_total: AtomicU64,
    pub sync_cycles_total: AtomicU64,
    pub sync_errors_total: AtomicU64,
}

pub type SharedMetrics = Arc<Metrics>;

impl Metrics {
    pub fn new() -> SharedMetrics {
        Arc::new(Self::default())
    }

    /// Render Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(1024);

        gauge(
            &mut out,
            "torrentng_torrents_total",
            "Total torrents in session",
            self.torrents_total.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "torrentng_torrents_seeding",
            "Torrents currently seeding",
            self.torrents_seeding.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "torrentng_torrents_downloading",
            "Torrents currently downloading",
            self.torrents_downloading.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "torrentng_torrents_stopped",
            "Torrents stopped",
            self.torrents_stopped.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "torrentng_torrents_errored",
            "Torrents in error state",
            self.torrents_errored.load(Ordering::Relaxed),
        );
        gauge(
            &mut out,
            "torrentng_peers_connected",
            "Connected peers across all torrents",
            self.peers_connected.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "torrentng_api_requests_total",
            "Total API requests served",
            self.api_requests_total.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "torrentng_sync_cycles_total",
            "Total rTorrent sync cycles completed",
            self.sync_cycles_total.load(Ordering::Relaxed),
        );
        counter(
            &mut out,
            "torrentng_sync_errors_total",
            "Total rTorrent sync cycle errors",
            self.sync_errors_total.load(Ordering::Relaxed),
        );
        out
    }
}

fn gauge(out: &mut String, name: &str, help: &str, value: i64) {
    out.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
    ));
}

fn counter(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!(
        "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"
    ));
}
