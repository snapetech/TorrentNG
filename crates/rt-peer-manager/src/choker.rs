/// Upload slot manager: BEP 3 choking algorithm (seeding variant).
///
/// For a seeding-first client we use upload-rate-based unchoke: the N peers
/// with the highest recent upload rate get unchoked. One extra optimistic
/// unchoke slot rotates every 30 seconds to give new peers a chance.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::peer::{ChokeState, PeerId};

/// Maximum regular unchoke slots (excludes the optimistic slot).
pub const DEFAULT_MAX_UNCHOKED: usize = 4;

/// Interval at which the optimistic unchoke slot rotates.
pub const OPTIMISTIC_ROTATE_INTERVAL: Duration = Duration::from_secs(30);

/// Decision produced by the choker for a single peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChokeDecision {
    Unchoke,
    Choke,
}

/// Lightweight snapshot of a peer used for choking decisions.
#[derive(Debug, Clone)]
pub struct PeerSnapshot {
    pub id: PeerId,
    pub interested: bool,
    pub upload_rate: f64,
    pub current_choke: ChokeState,
}

/// Choking algorithm state.
pub struct Choker {
    max_unchoked: usize,
    optimistic_id: Option<PeerId>,
    last_optimistic_rotate: Instant,
}

impl Choker {
    pub fn new(max_unchoked: usize) -> Self {
        Choker {
            max_unchoked,
            optimistic_id: None,
            last_optimistic_rotate: Instant::now(),
        }
    }

    /// Run one choking round. Returns a map of peer → decision.
    ///
    /// Only peers that are `interested` compete for unchoke slots.
    /// Peers that are not interested are always choked.
    pub fn run(&mut self, peers: &[PeerSnapshot]) -> HashMap<PeerId, ChokeDecision> {
        // Only interested peers compete.
        let mut interested: Vec<&PeerSnapshot> = peers.iter().filter(|p| p.interested).collect();

        // Sort by upload rate descending — highest throughput earns a slot.
        interested.sort_by(|a, b| b.upload_rate.partial_cmp(&a.upload_rate).unwrap());

        // Pick top N for regular slots.
        let unchoked_ids: Vec<PeerId> = interested
            .iter()
            .take(self.max_unchoked)
            .map(|p| p.id)
            .collect();

        // Rotate optimistic slot on interval.
        if self.last_optimistic_rotate.elapsed() >= OPTIMISTIC_ROTATE_INTERVAL
            || self.optimistic_id.is_none()
        {
            // Pick first interested peer NOT already in regular unchoked set.
            let candidate = interested
                .iter()
                .skip(self.max_unchoked)
                .map(|p| p.id)
                .next();
            if let Some(id) = candidate {
                self.optimistic_id = Some(id);
                self.last_optimistic_rotate = Instant::now();
            }
        }

        // Build decision map.
        let mut decisions = HashMap::new();
        for p in peers {
            let in_regular = unchoked_ids.contains(&p.id);
            let in_optimistic = self.optimistic_id == Some(p.id);
            let decision = if (in_regular || in_optimistic) && p.interested {
                ChokeDecision::Unchoke
            } else {
                ChokeDecision::Choke
            };
            decisions.insert(p.id, decision);
        }
        decisions
    }
}

impl Default for Choker {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_UNCHOKED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::ChokeState;

    fn snap(id: PeerId, interested: bool, rate: f64) -> PeerSnapshot {
        PeerSnapshot {
            id,
            interested,
            upload_rate: rate,
            current_choke: ChokeState::Choked,
        }
    }

    #[test]
    fn top_n_peers_unchoked() {
        let mut choker = Choker::new(2);
        let ids: Vec<PeerId> = (0..5).map(|_| PeerId::new()).collect();
        let peers = vec![
            snap(ids[0], true, 10.0),
            snap(ids[1], true, 50.0),   // best — always in regular slot
            snap(ids[2], true, 30.0),   // second — always in regular slot
            snap(ids[3], true, 5.0),    // may get optimistic on first run
            snap(ids[4], false, 100.0), // not interested
        ];
        let decisions = choker.run(&peers);
        // Top-2 by rate always unchoked.
        assert_eq!(decisions[&ids[1]], ChokeDecision::Unchoke);
        assert_eq!(decisions[&ids[2]], ChokeDecision::Unchoke);
        // Not interested → always choked regardless of rate.
        assert_eq!(decisions[&ids[4]], ChokeDecision::Choke);
        // ids[0] or ids[3] may receive the optimistic slot — both together cannot be unchoked.
        let extra_unchoked = [ids[0], ids[3]]
            .iter()
            .filter(|id| decisions[id] == ChokeDecision::Unchoke)
            .count();
        assert!(extra_unchoked <= 1, "at most one optimistic slot");
    }

    #[test]
    fn not_interested_always_choked() {
        let mut choker = Choker::new(4);
        let id = PeerId::new();
        let peers = vec![snap(id, false, 9999.0)];
        let decisions = choker.run(&peers);
        assert_eq!(decisions[&id], ChokeDecision::Choke);
    }

    #[test]
    fn fewer_peers_than_slots_all_unchoked() {
        let mut choker = Choker::new(4);
        let ids: Vec<PeerId> = (0..3).map(|_| PeerId::new()).collect();
        let peers: Vec<PeerSnapshot> = ids.iter().map(|&id| snap(id, true, 1.0)).collect();
        let decisions = choker.run(&peers);
        // All 3 should be unchoked (fewer than 4 slots)
        for id in &ids {
            assert_eq!(decisions[id], ChokeDecision::Unchoke);
        }
    }

    #[test]
    fn optimistic_slot_assigned() {
        let mut choker = Choker::new(1);
        let ids: Vec<PeerId> = (0..3).map(|_| PeerId::new()).collect();
        // ids[0] has best rate → regular slot
        // ids[1] or ids[2] should get optimistic
        let peers = vec![
            snap(ids[0], true, 100.0),
            snap(ids[1], true, 10.0),
            snap(ids[2], true, 5.0),
        ];
        let decisions = choker.run(&peers);
        // ids[0] is in regular slot
        assert_eq!(decisions[&ids[0]], ChokeDecision::Unchoke);
        // At least one of the remaining gets the optimistic slot
        let opt_unchoked = decisions[&ids[1]] == ChokeDecision::Unchoke
            || decisions[&ids[2]] == ChokeDecision::Unchoke;
        assert!(opt_unchoked);
    }
}
