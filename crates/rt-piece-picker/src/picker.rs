/// Piece selection strategy: rarest-first with sequential fallback for endgame.
///
/// Maintains a `wanted` bitset (pieces we still need) and delegates
/// ordering to the `Availability` map. Priority pieces (e.g., file head/tail
/// for streaming) bypass rarest-first and are selected first.
use crate::availability::Availability;

/// Maximum block size enforced by the picker (mirrors BEP 3).
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024;

/// A single block request (piece + byte range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockRequest {
    pub piece: u32,
    pub begin: u32,
    pub length: u32,
}

/// Tracks the download state of a single piece.
#[derive(Debug, Clone)]
struct PieceState {
    piece_length: u32,
    /// Blocks that have been requested but not yet received.
    requested: Vec<bool>,
    /// Blocks that have been received.
    received: Vec<bool>,
}

impl PieceState {
    fn new(piece_length: u32) -> Self {
        let n_blocks = n_blocks(piece_length);
        PieceState {
            piece_length,
            requested: vec![false; n_blocks],
            received: vec![false; n_blocks],
        }
    }

    fn next_unrequested(&self) -> Option<usize> {
        self.requested
            .iter()
            .zip(self.received.iter())
            .position(|(&req, &recv)| !req && !recv)
    }

    fn mark_requested(&mut self, block_idx: usize) {
        if let Some(r) = self.requested.get_mut(block_idx) {
            *r = true;
        }
    }

    fn mark_received(&mut self, block_idx: usize) {
        if let Some(r) = self.received.get_mut(block_idx) {
            *r = true;
        }
        if let Some(r) = self.requested.get_mut(block_idx) {
            *r = false; // clear request — no longer outstanding
        }
    }

    fn is_complete(&self) -> bool {
        self.received.iter().all(|&r| r)
    }

    fn block_request(&self, block_idx: usize) -> BlockRequest {
        let begin = block_idx as u32 * MAX_BLOCK_SIZE;
        let remaining = self.piece_length.saturating_sub(begin);
        let length = remaining.min(MAX_BLOCK_SIZE);
        BlockRequest {
            piece: 0,
            begin,
            length,
        } // caller sets .piece
    }
}

fn n_blocks(piece_length: u32) -> usize {
    piece_length.div_ceil(MAX_BLOCK_SIZE) as usize
}

/// The piece picker.
pub struct PiecePicker {
    piece_count: usize,
    default_piece_length: u32,
    last_piece_length: u32,
    /// Pieces we still need.
    wanted: Vec<bool>,
    /// In-progress pieces (piece index → state).
    in_progress: std::collections::HashMap<usize, PieceState>,
    /// Priority pieces (selected before rarest-first).
    priority: Vec<usize>,
    pub availability: Availability,
}

impl PiecePicker {
    pub fn new(piece_count: usize, default_piece_length: u32, last_piece_length: u32) -> Self {
        PiecePicker {
            piece_count,
            default_piece_length,
            last_piece_length,
            wanted: vec![true; piece_count],
            in_progress: std::collections::HashMap::new(),
            priority: Vec::new(),
            availability: Availability::new(piece_count),
        }
    }

    /// Mark a piece as already complete (from fastresume).
    pub fn mark_have(&mut self, piece: usize) {
        if piece < self.piece_count {
            self.wanted[piece] = false;
            self.in_progress.remove(&piece);
        }
    }

    /// Mark a piece as needed again after verification failed.
    pub fn reject_piece(&mut self, piece: usize) {
        if piece < self.piece_count {
            self.wanted[piece] = true;
            self.in_progress.remove(&piece);
        }
    }

    /// Set priority pieces (head/tail of each file for fast preview).
    pub fn set_priority(&mut self, pieces: Vec<usize>) {
        self.priority = pieces;
    }

    /// Pick the next block to request from a peer with given bitfield.
    ///
    /// Returns `None` if nothing is available from this peer right now.
    pub fn pick(&mut self, peer_has: &[bool]) -> Option<BlockRequest> {
        // Priority pieces first.
        for &p in &self.priority.clone() {
            if self.wanted[p] && peer_has.get(p).copied().unwrap_or(false) {
                if let Some(req) = self.pick_block_from(p) {
                    return Some(req);
                }
            }
        }

        // Rarest-first among wanted pieces the peer has.
        let ordered = self.availability.rarest_first(&self.wanted);
        for p in ordered {
            if peer_has.get(p).copied().unwrap_or(false) {
                if let Some(req) = self.pick_block_from(p) {
                    return Some(req);
                }
            }
        }
        None
    }

    fn piece_length_for(&self, piece: usize) -> u32 {
        if piece + 1 == self.piece_count {
            self.last_piece_length
        } else {
            self.default_piece_length
        }
    }

    fn pick_block_from(&mut self, piece: usize) -> Option<BlockRequest> {
        let pl = self.piece_length_for(piece);
        let state = self
            .in_progress
            .entry(piece)
            .or_insert_with(|| PieceState::new(pl));
        let block_idx = state.next_unrequested()?;
        state.mark_requested(block_idx);
        let mut req = state.block_request(block_idx);
        req.piece = piece as u32;
        Some(req)
    }

    /// Record a received block. Returns true if the piece is now complete.
    pub fn block_received(&mut self, piece: usize, begin: u32) -> bool {
        let block_idx = (begin / MAX_BLOCK_SIZE) as usize;
        if let Some(state) = self.in_progress.get_mut(&piece) {
            state.mark_received(block_idx);
            if state.is_complete() {
                self.in_progress.remove(&piece);
                self.wanted[piece] = false;
                return true;
            }
        }
        false
    }

    /// Cancel an outstanding block request (e.g., peer disconnected).
    pub fn cancel_request(&mut self, piece: usize, begin: u32) {
        let block_idx = (begin / MAX_BLOCK_SIZE) as usize;
        if let Some(state) = self.in_progress.get_mut(&piece) {
            if let Some(r) = state.requested.get_mut(block_idx) {
                *r = false;
            }
        }
    }

    pub fn is_complete(&self) -> bool {
        self.wanted.iter().all(|&w| !w)
    }

    pub fn remaining_pieces(&self) -> usize {
        self.wanted.iter().filter(|&&w| w).count()
    }

    pub fn bytes_left(&self) -> u64 {
        self.wanted
            .iter()
            .enumerate()
            .filter(|&(_, wanted)| *wanted)
            .map(|(piece, _)| self.piece_length_for(piece) as u64)
            .sum()
    }

    pub fn have_pieces(&self) -> Vec<bool> {
        self.wanted.iter().map(|wanted| !*wanted).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_1piece(piece_len: u32) -> PiecePicker {
        PiecePicker::new(1, piece_len, piece_len)
    }

    fn picker_4pieces(piece_len: u32, last_len: u32) -> PiecePicker {
        PiecePicker::new(4, piece_len, last_len)
    }

    fn peer_has_all(piece_count: usize) -> Vec<bool> {
        vec![true; piece_count]
    }

    #[test]
    fn picks_block_from_single_piece() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        let req = p.pick(&peer_has_all(1)).unwrap();
        assert_eq!(req.piece, 0);
        assert_eq!(req.begin, 0);
        assert_eq!(req.length, MAX_BLOCK_SIZE);
    }

    #[test]
    fn picks_second_block_after_first() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let r2 = p.pick(&all).unwrap();
        assert_ne!(r1.begin, r2.begin);
        // No third block
        assert!(p.pick(&all).is_none());
    }

    #[test]
    fn piece_complete_after_all_blocks_received() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let r2 = p.pick(&all).unwrap();
        assert!(!p.block_received(0, r1.begin));
        assert!(p.block_received(0, r2.begin));
        assert!(p.is_complete());
    }

    #[test]
    fn mark_have_removes_from_wanted() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        p.mark_have(0);
        p.mark_have(1);
        assert_eq!(p.remaining_pieces(), 2);
    }

    #[test]
    fn reject_piece_makes_completed_piece_wanted_again() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.mark_have(0);
        assert!(p.is_complete());
        p.reject_piece(0);
        assert!(!p.is_complete());
        assert_eq!(p.remaining_pieces(), 1);
    }

    #[test]
    fn bytes_left_sums_wanted_piece_lengths() {
        let mut p = picker_4pieces(10, 4);
        assert_eq!(p.bytes_left(), 34);
        p.mark_have(0);
        p.mark_have(3);
        assert_eq!(p.bytes_left(), 20);
    }

    #[test]
    fn have_pieces_is_inverse_of_wanted() {
        let mut p = picker_4pieces(10, 4);
        p.mark_have(1);
        p.mark_have(3);
        assert_eq!(p.have_pieces(), vec![false, true, false, true]);
    }

    #[test]
    fn mark_and_reject_update_recheck_accounting() {
        let mut p = picker_4pieces(10, 4);
        p.mark_have(0);
        p.mark_have(1);
        assert_eq!(p.bytes_left(), 14);
        p.reject_piece(1);
        assert_eq!(p.have_pieces(), vec![true, false, false, false]);
        assert_eq!(p.bytes_left(), 24);
    }

    #[test]
    fn cancel_request_makes_block_available_again() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let _r2 = p.pick(&all).unwrap();
        // Cancel r1 and pick again — should re-offer r1's block
        p.cancel_request(0, r1.begin);
        let r3 = p.pick(&all).unwrap();
        assert_eq!(r3.begin, r1.begin);
    }

    #[test]
    fn no_pick_when_peer_has_nothing() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        let no_has = vec![false];
        assert!(p.pick(&no_has).is_none());
    }

    #[test]
    fn priority_pieces_selected_first() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        // All pieces available from peer
        for i in 0..4 {
            p.availability.add_have(i);
        }
        // Piece 3 has lowest availability (set it manually)
        // Set priority to piece 2
        p.set_priority(vec![2]);
        let req = p.pick(&peer_has_all(4)).unwrap();
        assert_eq!(req.piece, 2);
    }

    #[test]
    fn last_piece_may_be_shorter() {
        // 3 full pieces + 1 half piece
        let full = MAX_BLOCK_SIZE;
        let half = MAX_BLOCK_SIZE / 2;
        let mut p = PiecePicker::new(4, full, half);
        for i in 0..4 {
            p.availability.add_have(i);
        }
        // Mark first 3 pieces done
        p.mark_have(0);
        p.mark_have(1);
        p.mark_have(2);
        // Only piece 3 (half length) remains
        let req = p.pick(&peer_has_all(4)).unwrap();
        assert_eq!(req.piece, 3);
        assert_eq!(req.length, half);
    }
}
