/// Tracks how many peers have each piece (availability count per piece).
///
/// Used by the rarest-first picker. Pieces are kept bucketed by their
/// current availability count so the picker can walk from the rarest
/// nonzero bucket upward instead of re-scanning every piece on each pick.
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct Availability {
    /// counts[i] = number of peers that have piece i.
    counts: Vec<u32>,
    /// buckets[c] holds every piece index currently at availability count
    /// `c`, in ascending piece-index order. Zero-availability pieces are
    /// never requestable so bucket 0 is never populated, but it is kept
    /// present once any bucket exists so indices line up 1:1 with counts.
    buckets: Vec<BTreeSet<u32>>,
    /// Pieces the owning picker has retired from consideration (already
    /// have, or permanently disabled) via `retire`. Retired pieces are kept
    /// out of every bucket regardless of swarm count, so a long-lived
    /// picker doesn't pay to skip over ever more completed pieces in a
    /// crowded bucket as a download approaches completion — a well-seeded
    /// swarm tends to give most remaining pieces nearly identical
    /// availability, so without this a near-finished download would
    /// otherwise drift back toward an O(piece_count) walk. `count()` is
    /// unaffected: retiring a piece never changes the swarm-wide count it
    /// reports.
    retired: Vec<bool>,
}

impl Availability {
    pub fn new(piece_count: usize) -> Self {
        Availability {
            counts: vec![0; piece_count],
            buckets: Vec::new(),
            retired: vec![false; piece_count],
        }
    }

    pub fn piece_count(&self) -> usize {
        self.counts.len()
    }

    /// Record that a peer has all pieces in `bitfield` (one bit per piece, MSB first).
    pub fn add_bitfield(&mut self, bitfield: &[u8]) {
        for (byte_idx, &byte) in bitfield.iter().enumerate() {
            for bit in 0..8 {
                let piece = byte_idx * 8 + bit;
                if piece >= self.counts.len() {
                    break;
                }
                if byte & (0x80 >> bit) != 0 {
                    self.add_have(piece);
                }
            }
        }
    }

    /// Record that a peer has a single piece (Have message).
    pub fn add_have(&mut self, piece: usize) {
        let Some(c) = self.counts.get_mut(piece) else {
            return;
        };
        let old = *c;
        let new = c.saturating_add(1);
        *c = new;
        if new != old && !self.retired.get(piece).copied().unwrap_or(false) {
            Self::bucket_remove(&mut self.buckets, old, piece as u32);
            Self::bucket_insert(&mut self.buckets, new, piece as u32);
        }
    }

    /// Remove one peer's ownership of a single piece.
    pub fn remove_have(&mut self, piece: usize) {
        let Some(c) = self.counts.get_mut(piece) else {
            return;
        };
        let old = *c;
        let new = c.saturating_sub(1);
        *c = new;
        if new != old && !self.retired.get(piece).copied().unwrap_or(false) {
            Self::bucket_remove(&mut self.buckets, old, piece as u32);
            Self::bucket_insert(&mut self.buckets, new, piece as u32);
        }
    }

    /// A peer disconnected; remove its bitfield from availability.
    pub fn remove_bitfield(&mut self, bitfield: &[u8]) {
        for (byte_idx, &byte) in bitfield.iter().enumerate() {
            for bit in 0..8 {
                let piece = byte_idx * 8 + bit;
                if piece >= self.counts.len() {
                    break;
                }
                if byte & (0x80 >> bit) != 0 {
                    self.remove_have(piece);
                }
            }
        }
    }

    pub fn count(&self, piece: usize) -> u32 {
        self.counts.get(piece).copied().unwrap_or(0)
    }

    /// Pieces sorted by availability ascending (rarest first).
    /// Only returns pieces for which `want[piece]` is true and count > 0.
    ///
    /// Walks the availability buckets (already grouped and ordered by
    /// count) instead of sorting every piece on each call.
    pub fn rarest_first(&self, want: &[bool]) -> Vec<usize> {
        self.buckets
            .iter()
            .skip(1) // bucket 0 is never populated (see field doc)
            .flat_map(|bucket| bucket.iter().copied())
            .map(|piece| piece as usize)
            .filter(|&piece| want.get(piece).copied().unwrap_or(false))
            .collect()
    }

    /// Walk availability buckets from the rarest nonzero count upward,
    /// returning the first piece index accepted by `predicate`.
    ///
    /// Within a bucket, pieces are visited in ascending index order, so for
    /// callers that treat "smallest (count, piece index)" as the winning
    /// candidate, this returns exactly that piece — the same tie-break a
    /// full linear scan comparing `(count, piece)` tuples would produce —
    /// without visiting pieces outside the matching bucket.
    pub(crate) fn rarest_matching<F>(&self, mut predicate: F) -> Option<usize>
    where
        F: FnMut(usize) -> bool,
    {
        for bucket in self.buckets.iter().skip(1) {
            for &piece in bucket {
                let piece = piece as usize;
                if predicate(piece) {
                    return Some(piece);
                }
            }
        }
        None
    }

    /// Pull a piece out of the bucket walk regardless of its swarm count.
    /// Used by the owning picker once a piece is no longer something it
    /// will ever request again (already have, or disabled). `count()`
    /// still reports the piece's true swarm-wide availability afterward —
    /// only bucket membership (and therefore `rarest_matching`/
    /// `rarest_first` visibility) is affected. Idempotent.
    pub(crate) fn retire(&mut self, piece: usize) {
        let Some(flag) = self.retired.get_mut(piece) else {
            return;
        };
        if *flag {
            return;
        }
        *flag = true;
        let count = self.count(piece);
        Self::bucket_remove(&mut self.buckets, count, piece as u32);
    }

    /// Reverse of `retire`: put a piece back into the bucket walk at its
    /// current swarm count. Used by the owning picker when a piece becomes
    /// wanted again (e.g. a failed recheck, or re-enabling a file).
    /// Idempotent.
    pub(crate) fn reinstate(&mut self, piece: usize) {
        let Some(flag) = self.retired.get_mut(piece) else {
            return;
        };
        if !*flag {
            return;
        }
        *flag = false;
        let count = self.count(piece);
        Self::bucket_insert(&mut self.buckets, count, piece as u32);
    }

    fn bucket_insert(buckets: &mut Vec<BTreeSet<u32>>, count: u32, piece: u32) {
        if count == 0 {
            return;
        }
        let idx = count as usize;
        if idx >= buckets.len() {
            buckets.resize_with(idx + 1, BTreeSet::new);
        }
        buckets[idx].insert(piece);
    }

    fn bucket_remove(buckets: &mut [BTreeSet<u32>], count: u32, piece: u32) {
        if count == 0 {
            return;
        }
        if let Some(bucket) = buckets.get_mut(count as usize) {
            bucket.remove(&piece);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_bitfield_increments_counts() {
        let mut av = Availability::new(8);
        // bitfield: 11110000 → pieces 0,1,2,3
        av.add_bitfield(&[0b1111_0000]);
        assert_eq!(av.count(0), 1);
        assert_eq!(av.count(3), 1);
        assert_eq!(av.count(4), 0);
    }

    #[test]
    fn add_have_increments_single() {
        let mut av = Availability::new(4);
        av.add_have(2);
        assert_eq!(av.count(2), 1);
        assert_eq!(av.count(0), 0);
    }

    #[test]
    fn remove_bitfield_decrements() {
        let mut av = Availability::new(8);
        av.add_bitfield(&[0xFF]);
        av.remove_bitfield(&[0xFF]);
        for i in 0..8 {
            assert_eq!(av.count(i), 0);
        }
    }

    #[test]
    fn remove_does_not_underflow() {
        let mut av = Availability::new(4);
        av.remove_bitfield(&[0xFF]); // should not panic or underflow
        assert_eq!(av.count(0), 0);
    }

    #[test]
    fn remove_have_decrements_single_piece() {
        let mut av = Availability::new(4);
        av.add_have(2);
        av.add_have(2);

        av.remove_have(2);

        assert_eq!(av.count(2), 1);
        assert_eq!(av.count(0), 0);
    }

    #[test]
    fn rarest_first_orders_ascending() {
        let mut av = Availability::new(4);
        // piece 0: 3 peers, piece 1: 1 peer, piece 2: 2 peers, piece 3: 0 peers
        for _ in 0..3 {
            av.add_have(0);
        }
        av.add_have(1);
        av.add_have(1);
        av.add_have(2);
        av.add_have(2);
        av.add_have(2);
        // Recount: 0→3, 1→2, 2→3, 3→0
        // Rarest first (excluding unavailable): piece 1 (2), then 0 or 2 (both 3)
        let want = vec![true, true, true, true];
        let order = av.rarest_first(&want);
        assert_eq!(order[0], 1); // rarest
        assert!(order.contains(&0));
        assert!(order.contains(&2));
        assert!(!order.contains(&3)); // count=0
    }

    #[test]
    fn rarest_first_respects_want_filter() {
        let mut av = Availability::new(4);
        av.add_have(0);
        av.add_have(1);
        let want = vec![true, false, false, false];
        let order = av.rarest_first(&want);
        assert_eq!(order, vec![0]);
    }

    #[test]
    fn bitfield_out_of_bounds_ignored() {
        let mut av = Availability::new(4);
        // Bitfield has 8 bits but only 4 pieces exist — extra bits silently ignored.
        av.add_bitfield(&[0xFF]);
        assert_eq!(av.piece_count(), 4);
        for i in 0..4 {
            assert_eq!(av.count(i), 1);
        }
    }

    #[test]
    fn rarest_matching_walks_buckets_in_rarity_then_index_order() {
        let mut av = Availability::new(4);
        av.add_have(0);
        av.add_have(0);
        av.add_have(1);
        av.add_have(2);
        av.add_have(2);
        // counts: 0→2, 1→1, 2→2, 3→0
        let mut visited = Vec::new();
        let found = av.rarest_matching(|piece| {
            visited.push(piece);
            piece == 2
        });
        // Rarest bucket (count 1) is piece 1 — visited but rejected.
        // Next bucket (count 2) is pieces {0, 2} in ascending order.
        assert_eq!(visited, vec![1, 0, 2]);
        assert_eq!(found, Some(2));
    }

    #[test]
    fn rarest_matching_skips_zero_availability_pieces() {
        let av = Availability::new(4);
        assert_eq!(av.rarest_matching(|_| true), None);
    }

    #[test]
    fn retire_removes_piece_from_bucket_walk_but_keeps_count() {
        let mut av = Availability::new(2);
        av.add_have(0);
        av.add_have(1);

        av.retire(0);

        assert_eq!(av.count(0), 1); // count() unaffected by retirement
        let mut visited = Vec::new();
        av.rarest_matching(|piece| {
            visited.push(piece);
            false
        });
        assert_eq!(visited, vec![1]);
    }

    #[test]
    fn reinstate_returns_a_retired_piece_to_its_current_bucket() {
        let mut av = Availability::new(1);
        av.add_have(0);
        av.retire(0);
        assert_eq!(av.rarest_matching(|_| true), None);

        av.reinstate(0);

        assert_eq!(av.rarest_matching(|_| true), Some(0));
    }

    #[test]
    fn retiring_a_piece_suppresses_further_bucket_moves_until_reinstated() {
        let mut av = Availability::new(1);
        av.add_have(0);
        av.retire(0);

        // Count keeps tracking swarm changes while retired...
        av.add_have(0);
        av.remove_have(0);
        assert_eq!(av.count(0), 1);

        // ...but the piece stays out of the walk until reinstated, then
        // reappears at its current (up to date) count.
        assert_eq!(av.rarest_matching(|_| true), None);
        av.reinstate(0);
        assert_eq!(av.rarest_matching(|_| true), Some(0));
    }

    #[test]
    fn retire_and_reinstate_are_idempotent() {
        let mut av = Availability::new(1);
        av.add_have(0);
        av.retire(0);
        av.retire(0); // no-op, must not double-remove or panic
        assert_eq!(av.rarest_matching(|_| true), None);

        av.reinstate(0);
        av.reinstate(0); // no-op
        assert_eq!(av.rarest_matching(|_| true), Some(0));
    }

    #[test]
    fn moving_between_buckets_does_not_leak_stale_membership() {
        let mut av = Availability::new(2);
        av.add_have(0);
        av.add_have(0); // count 2
        av.remove_have(0); // back to count 1
        let mut visited = Vec::new();
        av.rarest_matching(|piece| {
            visited.push(piece);
            false
        });
        // Piece 0 should show up exactly once (in the count-1 bucket), not
        // linger in a stale count-2 bucket entry.
        assert_eq!(visited, vec![0]);
    }
}
