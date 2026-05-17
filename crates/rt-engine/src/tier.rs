use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPieceBitmap {
    piece_count: u32,
    bytes: Vec<u8>,
}

impl CompactPieceBitmap {
    pub fn missing(piece_count: u32) -> Self {
        Self {
            piece_count,
            bytes: vec![0; (piece_count as usize).div_ceil(8)],
        }
    }

    pub fn from_pieces(pieces: &[bool]) -> Self {
        let mut bytes = vec![0u8; pieces.len().div_ceil(8)];
        for (idx, has_piece) in pieces.iter().copied().enumerate() {
            if has_piece {
                bytes[idx / 8] |= 1 << (7 - (idx % 8));
            }
        }
        Self {
            piece_count: pieces.len() as u32,
            bytes,
        }
    }

    pub fn from_bytes(piece_count: u32, bytes: Vec<u8>) -> Result<Self, String> {
        let expected_len = (piece_count as usize).div_ceil(8);
        if bytes.len() != expected_len {
            return Err(format!(
                "piece bitmap has {} bytes, expected {expected_len}",
                bytes.len()
            ));
        }
        if piece_count % 8 != 0 && !bytes.is_empty() {
            let used_bits = piece_count % 8;
            let unused_mask = (1 << (8 - used_bits)) - 1;
            if bytes[bytes.len() - 1] & unused_mask != 0 {
                return Err("piece bitmap has set bits beyond piece_count".to_owned());
            }
        }
        Ok(Self { piece_count, bytes })
    }

    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn has_piece(&self, piece: u32) -> bool {
        if piece >= self.piece_count {
            return false;
        }
        let idx = piece as usize;
        self.bytes[idx / 8] & (1 << (7 - (idx % 8))) != 0
    }

    pub fn complete_pieces(&self) -> u32 {
        if self.piece_count == 0 {
            return 0;
        }
        let mut total = 0;
        for piece in 0..self.piece_count {
            if self.has_piece(piece) {
                total += 1;
            }
        }
        total
    }

    pub fn estimated_heap_bytes(&self) -> usize {
        self.bytes.capacity()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DormantTorrentSnapshot {
    pub info_hash: String,
    pub state: TorrentState,
    pub pieces: CompactPieceBitmap,
    pub tracker_deadline: Option<Instant>,
    pub last_active: Option<Instant>,
}

impl DormantTorrentSnapshot {
    pub fn new(
        info_hash: impl Into<String>,
        state: TorrentState,
        pieces: CompactPieceBitmap,
        tracker_deadline: Option<Instant>,
        last_active: Option<Instant>,
    ) -> Self {
        Self {
            info_hash: info_hash.into(),
            state,
            pieces,
            tracker_deadline,
            last_active,
        }
    }

    pub fn estimate_heap_bytes(&self) -> usize {
        self.info_hash.capacity() + self.pieces.estimated_heap_bytes()
    }

    pub fn tier_input(&self, now: Instant, tracker_due_slack: Duration) -> TierInput {
        TierInput {
            state: self.state,
            connected_peers: 0,
            outstanding_requests: 0,
            inbound_peer: false,
            tracker_due: self
                .tracker_deadline
                .map(|deadline| deadline <= now + tracker_due_slack)
                .unwrap_or(false),
            last_active: self.last_active,
            now,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierEvent {
    InboundPeer,
    AnnounceDue,
    PeerDrained,
    RequestQueued,
    RequestDrained,
    StateChanged(TorrentState),
    IdleElapsed,
}

#[derive(Debug, Clone)]
pub struct TierController<K> {
    policy: TierPolicy,
    tiers: HashMap<K, TorrentActivityTier>,
    idle_checks: ActivityTimerWheel<K>,
}

impl<K> TierController<K>
where
    K: Clone + Eq + Hash,
{
    pub fn new(policy: TierPolicy) -> Self {
        Self {
            policy,
            tiers: HashMap::new(),
            idle_checks: ActivityTimerWheel::default(),
        }
    }

    pub fn tier(&self, key: &K) -> Option<TorrentActivityTier> {
        self.tiers.get(key).copied()
    }

    pub fn tracked_len(&self) -> usize {
        self.tiers.len()
    }

    pub fn next_idle_deadline(&self) -> Option<Instant> {
        self.idle_checks.next_deadline()
    }

    pub fn remove(&mut self, key: &K) -> Option<TorrentActivityTier> {
        self.idle_checks.cancel(key);
        self.tiers.remove(key)
    }

    pub fn apply_input(&mut self, key: K, input: TierInput) -> TierDecision {
        let decision = self.policy.decide(input);
        self.record_decision(key, input.now, decision);
        decision
    }

    pub fn apply_event(&mut self, key: K, mut input: TierInput, event: TierEvent) -> TierDecision {
        match event {
            TierEvent::InboundPeer => {
                input.inbound_peer = true;
                input.connected_peers = input.connected_peers.max(1);
                input.last_active = Some(input.now);
            }
            TierEvent::AnnounceDue => input.tracker_due = true,
            TierEvent::PeerDrained => input.connected_peers = 0,
            TierEvent::RequestQueued => {
                input.outstanding_requests = input.outstanding_requests.max(1)
            }
            TierEvent::RequestDrained => input.outstanding_requests = 0,
            TierEvent::StateChanged(state) => input.state = state,
            TierEvent::IdleElapsed => {}
        }
        self.apply_input(key, input)
    }

    pub fn pop_due_idle_checks(&mut self, now: Instant) -> Vec<K> {
        self.idle_checks.pop_due(now)
    }

    fn record_decision(&mut self, key: K, now: Instant, decision: TierDecision) {
        self.tiers.insert(key.clone(), decision.tier);
        if let Some(delay) = decision.next_idle_check {
            self.idle_checks.schedule(key, now + delay);
        } else {
            self.idle_checks.cancel(&key);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierScaleSnapshot {
    pub total_torrents: usize,
    pub hot_torrents: usize,
    pub warm_torrents: usize,
    pub dormant_torrents: usize,
    pub active_torrent_tasks: usize,
    pub dormant_heap_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierScaleBudget {
    pub max_hot_basis_points: usize,
    pub max_tasks_per_hot: usize,
    pub max_dormant_heap_bytes_per_torrent: usize,
}

impl Default for TierScaleBudget {
    fn default() -> Self {
        Self {
            max_hot_basis_points: 200,
            max_tasks_per_hot: 1,
            max_dormant_heap_bytes_per_torrent: 2 * 1024,
        }
    }
}

impl TierScaleSnapshot {
    pub fn validate(self, budget: TierScaleBudget) -> Result<(), String> {
        let tier_total = self
            .hot_torrents
            .saturating_add(self.warm_torrents)
            .saturating_add(self.dormant_torrents);
        if tier_total != self.total_torrents {
            return Err(format!(
                "tier total {tier_total} does not match total_torrents {}",
                self.total_torrents
            ));
        }

        let hot_scaled = self.hot_torrents.saturating_mul(10_000);
        let max_hot_scaled = self
            .total_torrents
            .saturating_mul(budget.max_hot_basis_points);
        if hot_scaled > max_hot_scaled {
            return Err(format!(
                "hot torrent share exceeds {}bp",
                budget.max_hot_basis_points
            ));
        }

        let max_tasks = self.hot_torrents.saturating_mul(budget.max_tasks_per_hot);
        if self.active_torrent_tasks > max_tasks {
            return Err(format!(
                "active torrent tasks {} exceeds budget {max_tasks}",
                self.active_torrent_tasks
            ));
        }

        let max_dormant_heap = self
            .dormant_torrents
            .saturating_mul(budget.max_dormant_heap_bytes_per_torrent);
        if self.dormant_heap_bytes > max_dormant_heap {
            return Err(format!(
                "dormant heap {} exceeds budget {max_dormant_heap}",
                self.dormant_heap_bytes
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ActivityTimerWheel<K> {
    deadlines: BTreeMap<Instant, Vec<K>>,
    scheduled: HashMap<K, Instant>,
}

impl<K> Default for ActivityTimerWheel<K>
where
    K: Clone + Eq + Hash,
{
    fn default() -> Self {
        Self {
            deadlines: BTreeMap::new(),
            scheduled: HashMap::new(),
        }
    }
}

impl<K> ActivityTimerWheel<K>
where
    K: Clone + Eq + Hash,
{
    pub fn len(&self) -> usize {
        self.scheduled.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scheduled.is_empty()
    }

    pub fn schedule(&mut self, key: K, deadline: Instant) {
        self.cancel(&key);
        self.deadlines
            .entry(deadline)
            .or_default()
            .push(key.clone());
        self.scheduled.insert(key, deadline);
    }

    pub fn cancel(&mut self, key: &K) -> bool {
        self.scheduled.remove(key).is_some()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.keys().next().copied()
    }

    pub fn pop_due(&mut self, now: Instant) -> Vec<K> {
        let due_deadlines: Vec<Instant> = self.deadlines.range(..=now).map(|(t, _)| *t).collect();
        let mut due = Vec::new();
        for deadline in due_deadlines {
            let Some(keys) = self.deadlines.remove(&deadline) else {
                continue;
            };
            for key in keys {
                if self.scheduled.get(&key).copied() == Some(deadline) {
                    self.scheduled.remove(&key);
                    due.push(key);
                }
            }
        }
        due
    }
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

    #[test]
    fn timer_wheel_pops_due_torrents_without_one_timer_per_entry() {
        let now = Instant::now();
        let mut wheel = ActivityTimerWheel::<String>::default();
        wheel.schedule("a".to_owned(), now + Duration::from_secs(10));
        wheel.schedule("b".to_owned(), now + Duration::from_secs(20));
        wheel.schedule("c".to_owned(), now + Duration::from_secs(20));

        assert_eq!(wheel.len(), 3);
        assert_eq!(wheel.next_deadline(), Some(now + Duration::from_secs(10)));
        assert!(wheel.pop_due(now + Duration::from_secs(9)).is_empty());

        assert_eq!(wheel.pop_due(now + Duration::from_secs(10)), vec!["a"]);
        assert_eq!(wheel.len(), 2);

        let mut due = wheel.pop_due(now + Duration::from_secs(20));
        due.sort();
        assert_eq!(due, vec!["b", "c"]);
        assert!(wheel.is_empty());
    }

    #[test]
    fn timer_wheel_reschedule_and_cancel_ignore_stale_slots() {
        let now = Instant::now();
        let mut wheel = ActivityTimerWheel::<String>::default();
        let torrent = "torrent".to_owned();
        wheel.schedule(torrent.clone(), now + Duration::from_secs(10));
        wheel.schedule(torrent.clone(), now + Duration::from_secs(30));
        assert!(wheel.pop_due(now + Duration::from_secs(10)).is_empty());
        assert_eq!(wheel.len(), 1);

        assert!(wheel.cancel(&torrent));
        assert!(wheel.pop_due(now + Duration::from_secs(30)).is_empty());
        assert!(wheel.is_empty());
    }

    #[test]
    fn compact_piece_bitmap_roundtrips_and_rejects_tail_bits() {
        let pieces = CompactPieceBitmap::from_pieces(&[
            true, false, true, false, false, false, false, false, true,
        ]);
        assert_eq!(pieces.piece_count(), 9);
        assert_eq!(pieces.bytes(), &[0b1010_0000, 0b1000_0000]);
        assert!(pieces.has_piece(0));
        assert!(!pieces.has_piece(1));
        assert!(pieces.has_piece(8));
        assert!(!pieces.has_piece(9));
        assert_eq!(pieces.complete_pieces(), 3);

        assert_eq!(
            CompactPieceBitmap::from_bytes(9, pieces.bytes().to_vec()).unwrap(),
            pieces
        );
        assert!(CompactPieceBitmap::from_bytes(9, vec![0, 0b0100_0000]).is_err());
        assert!(CompactPieceBitmap::from_bytes(9, vec![0]).is_err());
    }

    #[test]
    fn dormant_snapshot_keeps_only_bitmap_and_tracker_deadline_inputs() {
        let now = Instant::now();
        let snapshot = DormantTorrentSnapshot::new(
            "a".repeat(40),
            TorrentState::Seeding,
            CompactPieceBitmap::from_pieces(&[true; 10_000]),
            Some(now + Duration::from_secs(5)),
            Some(now - Duration::from_secs(3600)),
        );

        assert_eq!(snapshot.pieces.piece_count(), 10_000);
        assert!(snapshot.estimate_heap_bytes() < 2_000);

        let input = snapshot.tier_input(now, Duration::from_secs(10));
        assert!(input.tracker_due);
        assert_eq!(input.connected_peers, 0);
        assert_eq!(input.outstanding_requests, 0);
        assert!(!input.inbound_peer);
    }

    #[test]
    fn tier_controller_promotes_and_demotes_with_shared_idle_checks() {
        let now = Instant::now();
        let policy = TierPolicy {
            hot_idle: Duration::from_secs(10),
            warm_idle: Duration::from_secs(60),
        };
        let mut controller = TierController::new(policy);
        let key = "torrent".to_owned();
        let base = input(TorrentState::Seeding, now);

        let promoted = controller.apply_event(key.clone(), base, TierEvent::InboundPeer);
        assert_eq!(promoted.tier, TorrentActivityTier::Hot);
        assert_eq!(controller.tier(&key), Some(TorrentActivityTier::Hot));
        assert_eq!(
            controller.next_idle_deadline(),
            Some(now + Duration::from_secs(10))
        );

        let due = controller.pop_due_idle_checks(now + Duration::from_secs(10));
        assert_eq!(due, vec![key.clone()]);

        let mut idle = input(TorrentState::Seeding, now + Duration::from_secs(10));
        idle.last_active = Some(now);
        let warm = controller.apply_event(key.clone(), idle, TierEvent::PeerDrained);
        assert_eq!(warm.tier, TorrentActivityTier::Warm);

        let mut stale = input(TorrentState::Seeding, now + Duration::from_secs(70));
        stale.last_active = Some(now);
        let dormant = controller.apply_event(key.clone(), stale, TierEvent::IdleElapsed);
        assert_eq!(dormant.tier, TorrentActivityTier::Dormant);
        assert_eq!(controller.remove(&key), Some(TorrentActivityTier::Dormant));
        assert_eq!(controller.tracked_len(), 0);
    }

    #[test]
    fn scale_snapshot_enforces_100k_two_percent_proxy_budget() {
        let dormant_template = DormantTorrentSnapshot::new(
            "a".repeat(40),
            TorrentState::Seeding,
            CompactPieceBitmap::missing(8192),
            None,
            None,
        );
        let dormant_torrents = 98_000;
        let sample = TierScaleSnapshot {
            total_torrents: 100_000,
            hot_torrents: 2_000,
            warm_torrents: 0,
            dormant_torrents,
            active_torrent_tasks: 2_000,
            dormant_heap_bytes: dormant_template
                .estimate_heap_bytes()
                .saturating_mul(dormant_torrents),
        };

        sample.validate(TierScaleBudget::default()).unwrap();

        let too_many_tasks = TierScaleSnapshot {
            active_torrent_tasks: 2_001,
            ..sample
        };
        assert!(too_many_tasks.validate(TierScaleBudget::default()).is_err());

        let too_many_hot = TierScaleSnapshot {
            hot_torrents: 2_001,
            dormant_torrents: 97_999,
            ..sample
        };
        assert!(too_many_hot.validate(TierScaleBudget::default()).is_err());
    }
}
