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
    requested: Vec<u16>,
    /// Blocks that have been received.
    received: Vec<bool>,
}

impl PieceState {
    fn new(piece_length: u32) -> Self {
        let n_blocks = n_blocks(piece_length);
        PieceState {
            piece_length,
            requested: vec![0; n_blocks],
            received: vec![false; n_blocks],
        }
    }

    fn next_unrequested(&self) -> Option<usize> {
        self.requested
            .iter()
            .zip(self.received.iter())
            .position(|(&req, &recv)| req == 0 && !recv)
    }

    fn next_requested_not_received(&self, exclude: &[BlockRequest], piece: usize) -> Option<usize> {
        self.requested
            .iter()
            .zip(self.received.iter())
            .enumerate()
            .find_map(|(block_idx, (&req, &recv))| {
                if req == 0 || recv {
                    return None;
                }
                let candidate = self.block_request_for(piece, block_idx);
                (!exclude.contains(&candidate)).then_some(block_idx)
            })
    }

    fn mark_requested(&mut self, block_idx: usize) {
        if let Some(r) = self.requested.get_mut(block_idx) {
            *r = r.saturating_add(1);
        }
    }

    fn mark_received(&mut self, block_idx: usize) {
        if let Some(r) = self.received.get_mut(block_idx) {
            *r = true;
        }
        if let Some(r) = self.requested.get_mut(block_idx) {
            *r = 0; // clear requests — no longer outstanding
        }
    }

    fn is_complete(&self) -> bool {
        self.received.iter().all(|&r| r)
    }

    fn received_blocks(&self) -> Vec<u32> {
        self.received
            .iter()
            .enumerate()
            .filter_map(|(idx, received)| received.then_some(idx as u32))
            .collect()
    }

    fn block_request_for(&self, piece: usize, block_idx: usize) -> BlockRequest {
        let begin = block_idx as u32 * MAX_BLOCK_SIZE;
        let remaining = self.piece_length.saturating_sub(begin);
        let length = remaining.min(MAX_BLOCK_SIZE);
        BlockRequest {
            piece: piece as u32,
            begin,
            length,
        }
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
    /// Pieces eligible to request. Disabled pieces are skipped without being advertised as complete.
    enabled: Vec<bool>,
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
            enabled: vec![true; piece_count],
            in_progress: std::collections::HashMap::new(),
            priority: Vec::new(),
            availability: Availability::new(piece_count),
        }
    }

    /// Mark a piece as already complete (from fastresume).
    pub fn mark_have(&mut self, piece: usize) {
        if piece < self.piece_count {
            self.wanted[piece] = false;
            self.enabled[piece] = true;
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

    pub fn set_piece_enabled(&mut self, piece: usize, enabled: bool) {
        if piece < self.piece_count {
            self.enabled[piece] = enabled;
            if !enabled {
                self.in_progress.remove(&piece);
            }
        }
    }

    pub fn restore_partial_piece(&mut self, piece: usize, received_blocks: &[u32]) {
        if piece >= self.piece_count
            || received_blocks.is_empty()
            || !self.wanted[piece]
            || !self.enabled[piece]
        {
            return;
        }
        let piece_length = self.piece_length_for(piece);
        let mut state = PieceState::new(piece_length);
        for block_idx in received_blocks {
            if let Some(received) = state.received.get_mut(*block_idx as usize) {
                *received = true;
            }
        }
        if state.is_complete() {
            self.wanted[piece] = false;
            self.in_progress.remove(&piece);
        } else {
            self.in_progress.insert(piece, state);
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
            if self.wanted[p] && self.enabled[p] && peer_has.get(p).copied().unwrap_or(false) {
                if let Some(req) = self.pick_block_from(p) {
                    return Some(req);
                }
            }
        }

        // Rarest-first among wanted pieces the peer has.
        let wanted_enabled: Vec<bool> = self
            .wanted
            .iter()
            .zip(self.enabled.iter())
            .map(|(wanted, enabled)| *wanted && *enabled)
            .collect();
        let ordered = self.availability.rarest_first(&wanted_enabled);
        for p in ordered {
            if peer_has.get(p).copied().unwrap_or(false) {
                if let Some(req) = self.pick_block_from(p) {
                    return Some(req);
                }
            }
        }
        None
    }

    /// Pick a duplicate outstanding block for endgame mode.
    ///
    /// This returns `Some` only after all enabled wanted pieces have no fresh
    /// unrequested blocks left. The caller supplies blocks already outstanding
    /// with the peer so we do not duplicate the same block to one peer.
    pub fn pick_endgame(
        &mut self,
        peer_has: &[bool],
        already_requested_by_peer: &[BlockRequest],
    ) -> Option<BlockRequest> {
        if !self.endgame_active() {
            return None;
        }

        for piece in self.pieces_in_pick_order() {
            if !self.wanted[piece]
                || !self.enabled[piece]
                || !peer_has.get(piece).copied().unwrap_or(false)
            {
                continue;
            }

            let Some(state) = self.in_progress.get_mut(&piece) else {
                continue;
            };
            let Some(block_idx) =
                state.next_requested_not_received(already_requested_by_peer, piece)
            else {
                continue;
            };
            state.mark_requested(block_idx);
            return Some(state.block_request_for(piece, block_idx));
        }
        None
    }

    fn endgame_active(&self) -> bool {
        let has_wanted = self
            .wanted
            .iter()
            .zip(self.enabled.iter())
            .any(|(wanted, enabled)| *wanted && *enabled);
        has_wanted
            && self
                .wanted
                .iter()
                .zip(self.enabled.iter())
                .enumerate()
                .filter(|(_, (wanted, enabled))| **wanted && **enabled)
                .all(|(piece, _)| {
                    self.in_progress
                        .get(&piece)
                        .and_then(PieceState::next_unrequested)
                        .is_none()
                })
    }

    fn pieces_in_pick_order(&self) -> Vec<usize> {
        let mut ordered = Vec::new();
        for piece in self.priority.iter().copied() {
            if !ordered.contains(&piece) {
                ordered.push(piece);
            }
        }

        let wanted_enabled: Vec<bool> = self
            .wanted
            .iter()
            .zip(self.enabled.iter())
            .map(|(wanted, enabled)| *wanted && *enabled)
            .collect();
        for piece in self.availability.rarest_first(&wanted_enabled) {
            if !ordered.contains(&piece) {
                ordered.push(piece);
            }
        }
        ordered
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
        Some(state.block_request_for(piece, block_idx))
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
                *r = r.saturating_sub(1);
            }
        }
    }

    /// Drop all outstanding block request bookkeeping without marking data complete.
    pub fn reset_outstanding_requests(&mut self) {
        self.in_progress.clear();
    }

    pub fn is_complete(&self) -> bool {
        self.wanted
            .iter()
            .zip(self.enabled.iter())
            .all(|(wanted, enabled)| !*enabled || !*wanted)
    }

    pub fn remaining_pieces(&self) -> usize {
        self.wanted
            .iter()
            .zip(self.enabled.iter())
            .filter(|(wanted, enabled)| **wanted && **enabled)
            .count()
    }

    pub fn bytes_left(&self) -> u64 {
        self.wanted
            .iter()
            .enumerate()
            .filter(|&(piece, wanted)| *wanted && self.enabled[piece])
            .map(|(piece, _)| self.piece_length_for(piece) as u64)
            .sum()
    }

    pub fn have_pieces(&self) -> Vec<bool> {
        self.wanted.iter().map(|wanted| !*wanted).collect()
    }

    pub fn partial_pieces(&self) -> Vec<(u32, Vec<u32>)> {
        let mut partials: Vec<_> = self
            .in_progress
            .iter()
            .filter_map(|(piece, state)| {
                let blocks = state.received_blocks();
                (!blocks.is_empty()).then_some((*piece as u32, blocks))
            })
            .collect();
        partials.sort_by_key(|(piece, _)| *piece);
        partials
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
    fn partial_piece_snapshot_restores_received_blocks() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let first = p.pick(&all).unwrap();
        let second = p.pick(&all).unwrap();
        assert!(!p.block_received(0, first.begin));
        assert_eq!(p.partial_pieces(), vec![(0, vec![0])]);

        let mut restored = picker_1piece(MAX_BLOCK_SIZE * 2);
        restored.availability.add_have(0);
        restored.restore_partial_piece(0, &[0]);
        let next = restored.pick(&all).unwrap();
        assert_eq!(next.begin, second.begin);
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
    fn reset_outstanding_requests_makes_blocks_available_again() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let r2 = p.pick(&all).unwrap();
        assert!(p.pick(&all).is_none());

        p.reset_outstanding_requests();

        let again = p.pick(&all).unwrap();
        let next = p.pick(&all).unwrap();
        assert_eq!(again.begin, r1.begin);
        assert_eq!(next.begin, r2.begin);
    }

    #[test]
    fn endgame_duplicates_outstanding_blocks_after_fresh_work_is_exhausted() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let r2 = p.pick(&all).unwrap();

        assert!(p.pick(&all).is_none());
        let duplicate = p.pick_endgame(&all, &[]).unwrap();
        assert!(duplicate == r1 || duplicate == r2);
    }

    #[test]
    fn endgame_does_not_duplicate_same_block_to_same_peer() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let r1 = p.pick(&all).unwrap();
        let r2 = p.pick(&all).unwrap();

        let duplicate = p.pick_endgame(&all, &[r1]).unwrap();
        assert_eq!(duplicate, r2);
        assert!(p.pick_endgame(&all, &[r1, r2]).is_none());
    }

    #[test]
    fn no_pick_when_peer_has_nothing() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        let no_has = vec![false];
        assert!(p.pick(&no_has).is_none());
    }

    #[test]
    fn disabled_piece_is_not_requested_or_advertised_complete() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        p.set_piece_enabled(0, false);
        assert!(p.pick(&peer_has_all(1)).is_none());
        assert!(p.is_complete());
        assert_eq!(p.have_pieces(), vec![false]);
        assert_eq!(p.bytes_left(), 0);
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
