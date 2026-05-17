use std::time::{Duration, Instant};

use rt_session::TorrentState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TorrentActivityTier {
    Dormant,
    Warm,
    Hot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPolicy {
    pub warm_idle: Duration,
    pub hot_idle: Duration,
}

impl Default for TierPolicy {
    fn default() -> Self {
        Self {
            warm_idle: Duration::from_secs(30 * 60),
            hot_idle: Duration::from_secs(2 * 60),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TierInput {
    pub state: TorrentState,
    pub connected_peers: usize,
    pub outstanding_requests: usize,
    pub inbound_peer: bool,
    pub tracker_due: bool,
    pub last_active: Option<Instant>,
    pub now: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierDecision {
    pub tier: TorrentActivityTier,
    pub next_idle_check: Option<Duration>,
}

impl TierPolicy {
    pub fn decide(self, input: TierInput) -> TierDecision {
        if input.inbound_peer
            || input.connected_peers > 0
            || input.outstanding_requests > 0
            || matches!(
                input.state,
                TorrentState::Checking | TorrentState::Downloading | TorrentState::MetadataPending
            )
        {
            return TierDecision {
                tier: TorrentActivityTier::Hot,
                next_idle_check: Some(self.hot_idle),
            };
        }

        if input.state.is_terminal()
            || matches!(input.state, TorrentState::Stopped | TorrentState::Paused)
        {
            return TierDecision {
                tier: TorrentActivityTier::Dormant,
                next_idle_check: None,
            };
        }

        if input.tracker_due {
            return TierDecision {
                tier: TorrentActivityTier::Warm,
                next_idle_check: Some(self.warm_idle),
            };
        }

        let Some(last_active) = input.last_active else {
            return TierDecision {
                tier: TorrentActivityTier::Dormant,
                next_idle_check: None,
            };
        };

        let idle = input.now.saturating_duration_since(last_active);
        if idle < self.hot_idle {
            TierDecision {
                tier: TorrentActivityTier::Hot,
                next_idle_check: Some(self.hot_idle - idle),
            }
        } else if idle < self.warm_idle {
            TierDecision {
                tier: TorrentActivityTier::Warm,
                next_idle_check: Some(self.warm_idle - idle),
            }
        } else {
            TierDecision {
                tier: TorrentActivityTier::Dormant,
                next_idle_check: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(state: TorrentState, now: Instant) -> TierInput {
        TierInput {
            state,
            connected_peers: 0,
            outstanding_requests: 0,
            inbound_peer: false,
            tracker_due: false,
            last_active: None,
            now,
        }
    }

    #[test]
    fn tier_is_orthogonal_to_lifecycle_state_for_idle_seeds() {
        let now = Instant::now();
        let policy = TierPolicy {
            hot_idle: Duration::from_secs(10),
            warm_idle: Duration::from_secs(60),
        };
        let mut seed = input(TorrentState::Seeding, now);

        seed.last_active = Some(now - Duration::from_secs(5));
        assert_eq!(policy.decide(seed).tier, TorrentActivityTier::Hot);

        seed.last_active = Some(now - Duration::from_secs(30));
        assert_eq!(policy.decide(seed).tier, TorrentActivityTier::Warm);

        seed.last_active = Some(now - Duration::from_secs(120));
        assert_eq!(policy.decide(seed).tier, TorrentActivityTier::Dormant);
    }

    #[test]
    fn peer_activity_promotes_any_startable_state_to_hot() {
        let now = Instant::now();
        let policy = TierPolicy::default();
        for state in [
            TorrentState::Seeding,
            TorrentState::Queued,
            TorrentState::Paused,
            TorrentState::Stopped,
        ] {
            let mut active = input(state, now);
            active.inbound_peer = true;
            assert_eq!(policy.decide(active).tier, TorrentActivityTier::Hot);
        }
    }

    #[test]
    fn active_engine_work_stays_hot() {
        let now = Instant::now();
        let policy = TierPolicy::default();
        for state in [
            TorrentState::Checking,
            TorrentState::Downloading,
            TorrentState::MetadataPending,
        ] {
            assert_eq!(
                policy.decide(input(state, now)).tier,
                TorrentActivityTier::Hot
            );
        }
    }

    #[test]
    fn tracker_due_idle_seed_stays_warm_without_task_activity() {
        let now = Instant::now();
        let policy = TierPolicy::default();
        let mut seed = input(TorrentState::Seeding, now);
        seed.tracker_due = true;
        assert_eq!(policy.decide(seed).tier, TorrentActivityTier::Warm);
    }

    #[test]
    fn paused_stopped_and_error_default_to_dormant() {
        let now = Instant::now();
        let policy = TierPolicy::default();
        for state in [
            TorrentState::Paused,
            TorrentState::Stopped,
            TorrentState::Error,
        ] {
            assert_eq!(
                policy.decide(input(state, now)).tier,
                TorrentActivityTier::Dormant
            );
        }
    }
}
