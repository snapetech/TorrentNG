/// Tracks how many peers have each piece (availability count per piece).
///
/// Used by the rarest-first picker: sort pieces ascending by count.
#[derive(Debug, Clone)]
pub struct Availability {
    /// counts[i] = number of peers that have piece i.
    counts: Vec<u32>,
}

impl Availability {
    pub fn new(piece_count: usize) -> Self {
        Availability {
            counts: vec![0; piece_count],
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
                    self.counts[piece] = self.counts[piece].saturating_add(1);
                }
            }
        }
    }

    /// Record that a peer has a single piece (Have message).
    pub fn add_have(&mut self, piece: usize) {
        if let Some(c) = self.counts.get_mut(piece) {
            *c = c.saturating_add(1);
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
                    self.counts[piece] = self.counts[piece].saturating_sub(1);
                }
            }
        }
    }

    pub fn count(&self, piece: usize) -> u32 {
        self.counts.get(piece).copied().unwrap_or(0)
    }

    /// Pieces sorted by availability ascending (rarest first).
    /// Only returns pieces for which `want[piece]` is true and count > 0.
    pub fn rarest_first(&self, want: &[bool]) -> Vec<usize> {
        let mut pieces: Vec<(u32, usize)> = self
            .counts
            .iter()
            .enumerate()
            .filter(|&(i, &c)| c > 0 && want.get(i).copied().unwrap_or(false))
            .map(|(i, &c)| (c, i))
            .collect();
        pieces.sort_unstable();
        pieces.into_iter().map(|(_, i)| i).collect()
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
}
