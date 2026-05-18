use crate::state::{DEFAULT_INITIAL_WINDOW_BYTES, DEFAULT_MTU_PAYLOAD_BYTES};

pub const TARGET_DELAY_US: u32 = 100_000;
pub const MIN_CONGESTION_WINDOW_BYTES: u32 = DEFAULT_MTU_PAYLOAD_BYTES as u32;
pub const MAX_CONGESTION_WINDOW_BYTES: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelaySample {
    pub timestamp_diff_us: u32,
    pub bytes_acked: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtpCongestionController {
    cwnd_bytes: u32,
    base_delay_us: Option<u32>,
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
            base_delay_us: None,
            current_delay_us: None,
            target_delay_us: target_delay_us.max(1),
        }
    }

    pub fn cwnd_bytes(&self) -> u32 {
        self.cwnd_bytes
    }

    pub fn base_delay_us(&self) -> Option<u32> {
        self.base_delay_us
    }

    pub fn current_delay_us(&self) -> Option<u32> {
        self.current_delay_us
    }

    pub fn on_ack(&mut self, sample: DelaySample) {
        let delay = sample.timestamp_diff_us;
        self.base_delay_us = Some(self.base_delay_us.map_or(delay, |base| base.min(delay)));
        self.current_delay_us = Some(delay);

        let queuing_delay = delay.saturating_sub(self.base_delay_us.unwrap_or(delay));
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
        cc.on_ack(DelaySample {
            timestamp_diff_us: 20_000,
            bytes_acked: 1_000,
        });
        assert!(cc.cwnd_bytes() > 10_000);
        assert_eq!(cc.base_delay_us(), Some(20_000));
    }

    #[test]
    fn ack_above_target_reduces_window() {
        let mut cc = UtpCongestionController::new(10_000, TARGET_DELAY_US);
        cc.on_ack(DelaySample {
            timestamp_diff_us: 20_000,
            bytes_acked: 1_000,
        });
        let before = cc.cwnd_bytes();
        cc.on_ack(DelaySample {
            timestamp_diff_us: 250_000,
            bytes_acked: 1_000,
        });
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
}
