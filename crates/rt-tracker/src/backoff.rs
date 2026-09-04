use std::time::Duration;

use rand::RngExt;

/// Exponential backoff with jitter for tracker retry logic.
///
/// Also used for announce scheduling to prevent storms when loading
/// 10k+ torrents simultaneously.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
    jitter_fraction: f64,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Backoff {
            base,
            max,
            attempt: 0,
            jitter_fraction: 0.2,
        }
    }

    /// Tracker retry backoff: starts at 60s, doubles, caps at 1800s.
    pub fn tracker_retry() -> Self {
        Self::new(Duration::from_secs(60), Duration::from_secs(1800))
    }

    /// Reset after a successful announce.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Next backoff duration with ±jitter applied.
    pub fn next_delay(&mut self) -> Duration {
        let base_secs = self.base.as_secs_f64() * 2f64.powi(self.attempt as i32);
        let capped = base_secs.min(self.max.as_secs_f64());
        let jitter = rand::rng().random_range(
            capped * (1.0 - self.jitter_fraction)..=capped * (1.0 + self.jitter_fraction),
        );
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_secs_f64(jitter.min(self.max.as_secs_f64()))
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

/// Apply random jitter to an announce interval to spread out announces
/// when many torrents are loaded at once.
///
/// Jitter is ±`fraction` of the interval. For 15k torrents with a 30-min
/// interval, this spreads announces over ±6 minutes.
pub fn jitter_interval(interval: Duration, fraction: f64) -> Duration {
    let secs = interval.as_secs_f64();
    let delta = secs * fraction;
    let jittered = rand::rng().random_range((secs - delta)..=(secs + delta));
    Duration::from_secs_f64(jittered.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_with_attempts() {
        let mut b = Backoff::new(Duration::from_secs(10), Duration::from_secs(300));
        let d0 = b.next_delay();
        let d1 = b.next_delay();
        let d2 = b.next_delay();
        // Each attempt should be roughly double the previous (modulo jitter)
        // We just check they're increasing in expectation and bounded
        assert!(d0.as_secs_f64() > 0.0);
        assert!(d1.as_secs_f64() >= d0.as_secs_f64() * 0.5); // jitter can reduce
        let _ = d2; // produced without panic
    }

    #[test]
    fn backoff_capped_at_max() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(120));
        for _ in 0..20 {
            let d = b.next_delay();
            // Never exceeds max (with jitter ceiling)
            assert!(d <= Duration::from_secs(145)); // 120 * 1.2 jitter
        }
    }

    #[test]
    fn backoff_reset_returns_to_base() {
        let mut b = Backoff::new(Duration::from_secs(10), Duration::from_secs(300));
        b.next_delay();
        b.next_delay();
        b.next_delay();
        assert_eq!(b.attempt(), 3);
        b.reset();
        assert_eq!(b.attempt(), 0);
    }

    #[test]
    fn jitter_stays_positive() {
        let interval = Duration::from_secs(1800);
        for _ in 0..50 {
            let j = jitter_interval(interval, 0.2);
            assert!(j.as_secs() >= 1);
            assert!(j.as_secs() <= 2200); // 1800 * 1.2 = 2160
        }
    }

    #[test]
    fn jitter_spreads_announces() {
        // Verify that 100 jittered intervals are not all equal (storm prevention)
        let interval = Duration::from_secs(1800);
        let values: Vec<u64> = (0..100)
            .map(|_| jitter_interval(interval, 0.2).as_secs())
            .collect();
        let unique: std::collections::HashSet<_> = values.iter().collect();
        // At least several distinct values — not all the same second
        assert!(
            unique.len() > 5,
            "jitter should produce spread: got {}",
            unique.len()
        );
    }
}
