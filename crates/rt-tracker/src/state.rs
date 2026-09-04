use std::time::{Duration, Instant};

use crate::{
    backoff::{jitter_interval, Backoff},
    error::TrackerError,
    response::{AnnounceResponse, TrackerStatus},
};

/// Per-tracker state machine.
///
/// Tracks the announce schedule, retry state, and last known status
/// for a single tracker URL.
#[derive(Debug)]
pub struct TrackerState {
    pub url: String,
    pub status: TrackerStatus,
    pub last_announce: Option<Instant>,
    pub next_announce: Option<Instant>,
    pub last_success: Option<Instant>,
    pub interval: Duration,
    pub min_interval: Duration,
    /// Opaque tracker-provided bytes, retained for subsequent announces.
    pub tracker_id: Option<Vec<u8>>,
    pub scrape_complete: Option<u32>,
    pub scrape_incomplete: Option<u32>,
    pub scrape_downloaded: Option<u32>,
    backoff: Backoff,
    consecutive_failures: u32,
}

impl TrackerState {
    pub fn new(url: impl Into<String>) -> Self {
        TrackerState {
            url: url.into(),
            status: TrackerStatus::NeverAnnounced,
            last_announce: None,
            next_announce: None,
            last_success: None,
            interval: Duration::from_secs(1800),
            min_interval: Duration::from_secs(60),
            tracker_id: None,
            scrape_complete: None,
            scrape_incomplete: None,
            scrape_downloaded: None,
            backoff: Backoff::tracker_retry(),
            consecutive_failures: 0,
        }
    }

    /// Record a successful announce response.
    pub fn on_success(&mut self, resp: &AnnounceResponse) {
        let now = Instant::now();
        self.last_announce = Some(now);
        self.last_success = Some(now);
        self.consecutive_failures = 0;
        self.backoff.reset();

        self.interval = Duration::from_secs(resp.interval as u64);
        if let Some(mi) = resp.min_interval {
            self.min_interval = Duration::from_secs(mi as u64);
        }
        if let Some(ref id) = resp.tracker_id {
            self.tracker_id = Some(id.clone());
        }
        self.scrape_complete = resp.complete;
        self.scrape_incomplete = resp.incomplete;

        // A tracker-provided `min interval` is a lower bound, not merely
        // informational metadata. Keep jitter for herd avoidance, but do
        // not let it schedule an announce before that lower bound.
        let minimum = self.interval.max(self.min_interval);
        let jittered = jitter_interval(minimum, 0.1).max(self.min_interval);
        self.next_announce = Some(now + jittered);

        self.status = if let Some(ref warn) = resp.warning_message {
            TrackerStatus::Warning(warn.clone())
        } else {
            TrackerStatus::Working
        };
    }

    /// Record a failed announce.
    pub fn on_failure(&mut self, err: TrackerError) {
        self.last_announce = Some(Instant::now());
        self.consecutive_failures += 1;
        let delay = self.backoff.next_delay();
        self.next_announce = Some(Instant::now() + delay);
        self.status = TrackerStatus::Error(err);
    }

    /// True if it's time (or past time) to announce again.
    pub fn is_due(&self) -> bool {
        match self.next_announce {
            None => true,
            Some(when) => Instant::now() >= when,
        }
    }

    /// Time until next announce (0 if overdue).
    pub fn time_until_next(&self) -> Duration {
        match self.next_announce {
            None => Duration::ZERO,
            Some(when) => when.saturating_duration_since(Instant::now()),
        }
    }

    /// Force an immediate re-announce (e.g., user-triggered).
    pub fn schedule_immediate(&mut self) {
        self.next_announce = Some(Instant::now());
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_response(interval: u32, warning: Option<&str>) -> AnnounceResponse {
        AnnounceResponse {
            interval,
            min_interval: Some(60),
            peers: Vec::new(),
            tracker_id: None,
            warning_message: warning.map(|s| s.to_owned()),
            complete: Some(10),
            incomplete: Some(2),
        }
    }

    #[test]
    fn initial_state_is_due() {
        let ts = TrackerState::new("http://tracker.example.com/announce");
        assert_eq!(ts.status, TrackerStatus::NeverAnnounced);
        assert!(ts.is_due());
    }

    #[test]
    fn after_success_not_immediately_due() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        let resp = mock_response(1800, None);
        ts.on_success(&resp);
        assert_eq!(ts.status, TrackerStatus::Working);
        assert!(!ts.is_due());
        assert!(ts.time_until_next() > Duration::ZERO);
    }

    #[test]
    fn warning_sets_warning_status() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_success(&mock_response(1800, Some("peer limit exceeded")));
        assert!(matches!(ts.status, TrackerStatus::Warning(_)));
    }

    #[test]
    fn failure_schedules_backoff() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_failure(TrackerError::Timeout);
        assert!(matches!(ts.status, TrackerStatus::Error(_)));
        assert!(!ts.is_due()); // backoff applied
        assert_eq!(ts.consecutive_failures(), 1);
    }

    #[test]
    fn multiple_failures_increase_backoff() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_failure(TrackerError::Timeout);
        let wait1 = ts.time_until_next();
        // Force next_announce to now so we can call on_failure again
        ts.next_announce = Some(Instant::now());
        ts.on_failure(TrackerError::Timeout);
        let wait2 = ts.time_until_next();
        assert!(wait2 >= wait1 / 2); // second backoff >= half first (jitter)
        assert_eq!(ts.consecutive_failures(), 2);
    }

    #[test]
    fn success_resets_failure_count() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_failure(TrackerError::Timeout);
        ts.on_failure(TrackerError::Timeout);
        assert_eq!(ts.consecutive_failures(), 2);
        ts.on_success(&mock_response(1800, None));
        assert_eq!(ts.consecutive_failures(), 0);
    }

    #[test]
    fn schedule_immediate_makes_it_due() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_success(&mock_response(1800, None));
        assert!(!ts.is_due());
        ts.schedule_immediate();
        assert!(ts.is_due());
    }

    #[test]
    fn interval_set_from_response() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_success(&mock_response(3600, None));
        assert_eq!(ts.interval, Duration::from_secs(3600));
    }

    #[test]
    fn success_retains_opaque_tracker_id() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        let mut response = mock_response(1800, None);
        response.tracker_id = Some(vec![0x00, 0xff, 0x80]);

        ts.on_success(&response);

        assert_eq!(ts.tracker_id, Some(vec![0x00, 0xff, 0x80]));
    }

    #[test]
    fn response_min_interval_is_enforced() {
        let mut ts = TrackerState::new("http://tracker.example.com/announce");
        ts.on_success(&AnnounceResponse {
            interval: 1,
            min_interval: Some(60),
            peers: Vec::new(),
            tracker_id: None,
            warning_message: None,
            complete: None,
            incomplete: None,
        });

        assert!(ts.time_until_next() >= Duration::from_secs(59));
    }
}
