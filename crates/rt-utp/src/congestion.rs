use crate::state::{DEFAULT_INITIAL_WINDOW_BYTES, DEFAULT_MTU_PAYLOAD_BYTES};

pub const TARGET_DELAY_US: u32 = 100_000;
pub const MIN_CONGESTION_WINDOW_BYTES: u32 = DEFAULT_MTU_PAYLOAD_BYTES as u32;
pub const MAX_CONGESTION_WINDOW_BYTES: u32 = 16 * 1024 * 1024;

/// Number of sliding time buckets used to track the LEDBAT base delay.
/// A single lifetime-minimum base delay can never rise again once set, so
/// a genuine path change (route change, Wi-Fi roam, mid-transfer VPN hop)
/// that raises the true minimum one-way delay would be read as permanent
/// queuing forever after, collapsing the window and starving the
/// connection until it is torn down and re-established. Splitting the
/// minimum across several rotating time buckets — the same shape libutp
/// and other LEDBAT implementations use — lets the estimate track a
/// genuine increase within roughly one bucket duration while still
/// filtering sample-to-sample jitter within a bucket.
const BASE_DELAY_BUCKET_COUNT: usize = 3;
/// Each bucket covers this many microseconds, so the base delay forgets a
/// stale minimum after `BASE_DELAY_BUCKET_COUNT * BASE_DELAY_BUCKET_DURATION_US`
/// (here, a 3-minute rolling window).
const BASE_DELAY_BUCKET_DURATION_US: u32 = 60_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelaySample {
    pub timestamp_diff_us: u32,
    pub bytes_acked: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtpCongestionController {
    cwnd_bytes: u32,
    base_delay_buckets: [Option<u32>; BASE_DELAY_BUCKET_COUNT],
    base_delay_bucket_started_us: Option<u32>,
    current_delay_us: Option<u32>,
    target_delay_us: u32,
}

impl Default for UtpCongestionController {
    fn default() -> Self {
        Self::new(DEFAULT_INITIAL_WINDOW_BYTES, TARGET_DELAY_US)
    }
}

impl UtpCongestionController {
    pub fn new(initial_window_bytes: u32, target_delay_us: u32) -> Self {
        Self {
            cwnd_bytes: initial_window_bytes
                .clamp(MIN_CONGESTION_WINDOW_BYTES, MAX_CONGESTION_WINDOW_BYTES),
            base_delay_buckets: [None; BASE_DELAY_BUCKET_COUNT],
            base_delay_bucket_started_us: None,
            current_delay_us: None,
            target_delay_us: target_delay_us.max(1),
        }
    }

    pub fn cwnd_bytes(&self) -> u32 {
        self.cwnd_bytes
    }

    /// The windowed-minimum base (no-queuing) delay, or `None` before the
    /// first sample. `now_us` uses the same wrapping 32-bit microsecond
    /// clock as the rest of the uTP wire protocol.
    pub fn base_delay_us(&self) -> Option<u32> {
        self.base_delay_buckets
            .iter()
            .filter_map(|bucket| *bucket)
            .min()
    }

    pub fn current_delay_us(&self) -> Option<u32> {
        self.current_delay_us
    }

    fn record_base_delay_sample(&mut self, now_us: u32, delay: u32) {
        match self.base_delay_bucket_started_us {
            None => {
                self.base_delay_bucket_started_us = Some(now_us);
            }
            Some(started) => {
                let elapsed = now_us.wrapping_sub(started);
                if elapsed >= BASE_DELAY_BUCKET_DURATION_US {
                    let buckets_elapsed = elapsed / BASE_DELAY_BUCKET_DURATION_US;
                    let rotate_by = (buckets_elapsed as usize).min(BASE_DELAY_BUCKET_COUNT);
                    // Rotating left drops the oldest `rotate_by` buckets off
                    // the front and wraps their slots to the end, where the
                    // loop below clears them to start fresh (empty) buckets.
                    self.base_delay_buckets.rotate_left(rotate_by);
                    for bucket in self.base_delay_buckets.iter_mut().rev().take(rotate_by) {
                        *bucket = None;
                    }
                    self.base_delay_bucket_started_us =
                        Some(started.wrapping_add(
                            buckets_elapsed.wrapping_mul(BASE_DELAY_BUCKET_DURATION_US),
                        ));
                }
            }
        }
        let current = self
            .base_delay_buckets
            .last_mut()
            .expect("bucket array is non-empty");
        *current = Some(current.map_or(delay, |min| min.min(delay)));
    }

    pub fn on_ack(&mut self, now_us: u32, sample: DelaySample) {
        let delay = sample.timestamp_diff_us;
        self.record_base_delay_sample(now_us, delay);
        self.current_delay_us = Some(delay);

        // Duplicate and handshake ACKs still provide a delay sample, but they
        // did not retire any outgoing data. They must not grow or shrink the
        // congestion window as if a packet had been acknowledged.
        if sample.bytes_acked == 0 {
            return;
        }

        let queuing_delay = delay.saturating_sub(self.base_delay_us().unwrap_or(delay));
        if queuing_delay <= self.target_delay_us {
            let headroom = self.target_delay_us - queuing_delay;
            let gain = (u64::from(sample.bytes_acked.max(1)) * u64::from(headroom)
                / u64::from(self.target_delay_us))
            .max(1);
            self.cwnd_bytes = self
                .cwnd_bytes
                .saturating_add(gain.min(u64::from(u32::MAX)) as u32)
                .clamp(MIN_CONGESTION_WINDOW_BYTES, MAX_CONGESTION_WINDOW_BYTES);
        } else {
            let overshoot = queuing_delay - self.target_delay_us;
            let reduction = (u64::from(self.cwnd_bytes) * u64::from(overshoot)
                / u64::from(queuing_delay))
            .max(u64::from(DEFAULT_MTU_PAYLOAD_BYTES as u32));
            self.cwnd_bytes = self
                .cwnd_bytes
                .saturating_sub(reduction.min(u64::from(u32::MAX)) as u32)
                .clamp(MIN_CONGESTION_WINDOW_BYTES, MAX_CONGESTION_WINDOW_BYTES);
        }
    }

    pub fn on_timeout(&mut self) {
        self.cwnd_bytes = (self.cwnd_bytes / 2).max(MIN_CONGESTION_WINDOW_BYTES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_below_target_increases_window() {
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_ack(
            0,
            DelaySample {
                timestamp_diff_us: 20_000,
                bytes_acked: 1_000,
            },
        );
        assert!(cc.cwnd_bytes() > 10_000);
        assert_eq!(cc.base_delay_us(), Some(20_000));
    }

    #[test]
    fn ack_above_target_reduces_window() {
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_ack(
            0,
            DelaySample {
                timestamp_diff_us: 20_000,
                bytes_acked: 1_000,
            },
        );
        let before = cc.cwnd_bytes();
        cc.on_ack(
            1_000,
            DelaySample {
                timestamp_diff_us: 250_000,
                bytes_acked: 1_000,
            },
        );
        assert!(cc.cwnd_bytes() < before);
        assert_eq!(cc.base_delay_us(), Some(20_000));
        assert_eq!(cc.current_delay_us(), Some(250_000));
    }

    #[test]
    fn timeout_halves_window_but_keeps_mtu_floor() {
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_timeout();
        assert_eq!(cc.cwnd_bytes(), 5_000);
        for _ in 0..10 {
            cc.on_timeout();
        }
        assert_eq!(cc.cwnd_bytes(), MIN_CONGESTION_WINDOW_BYTES);
    }

    #[test]
    fn zero_byte_ack_records_delay_without_changing_window() {
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_ack(
            0,
            DelaySample {
                timestamp_diff_us: 20_000,
                bytes_acked: 0,
            },
        );

        assert_eq!(cc.cwnd_bytes(), 10_000);
        assert_eq!(cc.base_delay_us(), Some(20_000));
        assert_eq!(cc.current_delay_us(), Some(20_000));
    }

    #[test]
    fn base_delay_rises_after_a_sustained_path_change() {
        // A permanent lifetime-minimum base delay would read every future
        // 80ms sample as ~80ms of pure queuing forever after a route change
        // raised the true minimum from ~20ms to ~80ms, collapsing the
        // window and never recovering. The windowed minimum must instead
        // forget the stale 20ms floor once it ages out of every bucket.
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_ack(
            0,
            DelaySample {
                timestamp_diff_us: 20_000,
                bytes_acked: 1_000,
            },
        );
        assert_eq!(cc.base_delay_us(), Some(20_000));

        // Advance well past the full rolling window (3 buckets * 60s) with
        // a consistently higher delay, simulating a path change.
        let window_us = BASE_DELAY_BUCKET_COUNT as u32 * BASE_DELAY_BUCKET_DURATION_US;
        let mut now_us = 0u32;
        for _ in 0..8 {
            now_us = now_us.wrapping_add(window_us / 4);
            cc.on_ack(
                now_us,
                DelaySample {
                    timestamp_diff_us: 80_000,
                    bytes_acked: 1_000,
                },
            );
        }

        assert_eq!(
            cc.base_delay_us(),
            Some(80_000),
            "stale pre-path-change minimum must age out of the window"
        );
    }
}
