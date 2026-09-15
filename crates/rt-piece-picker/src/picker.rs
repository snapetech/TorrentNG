/// Piece selection strategy: rarest-first with optional sequential mode.
///
/// Maintains a `wanted` bitset (pieces we still need) and delegates
/// ordering to the `Availability` map. Priority pieces (e.g., file head/tail
/// for streaming) bypass rarest-first and are selected first.
use crate::availability::Availability;

/// Maximum block size enforced by the picker (mirrors BEP 3).
pub const MAX_BLOCK_SIZE: u32 = 16 * 1024;

/// Maximum number of distinct piece states retained while requests are in
/// flight or partial data is being restored. This matches the fast-resume
/// partial-piece admission limit so live state cannot exceed what can be
/// persisted and loaded safely.
pub const MAX_IN_PROGRESS_PIECES: usize = 16_384;

/// Maximum estimated backing storage for all in-progress piece states in one
/// picker. The estimate is intentionally conservative for `Vec<bool>` so a
/// large protocol-legal piece length cannot multiply into an unbounded
/// per-torrent allocation when many partial pieces are restored.
pub const MAX_IN_PROGRESS_PIECE_STATE_BYTES: usize = 64 * 1024 * 1024;

/// A single block request (piece + byte range).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockRequest {
    pub piece: u32,
    pub begin: u32,
    pub length: u32,
}

/// Read-only piece availability supplied by a peer. Keeping this as a small
/// trait lets engine-owned packed bitmaps participate in piece selection
/// without expanding them into one `bool` per piece for every request fill.
pub trait PieceAvailability {
    fn has_piece(&self, piece: usize) -> bool;
}

impl PieceAvailability for [bool] {
    fn has_piece(&self, piece: usize) -> bool {
        self.get(piece).copied().unwrap_or(false)
    }
}

impl PieceAvailability for Vec<bool> {
    fn has_piece(&self, piece: usize) -> bool {
        self.as_slice().has_piece(piece)
    }
}

impl<const N: usize> PieceAvailability for [bool; N] {
    fn has_piece(&self, piece: usize) -> bool {
        self.as_slice().has_piece(piece)
    }
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

    fn received_blocks_limited(&self, maximum: usize) -> Vec<u32> {
        self.received
            .iter()
            .enumerate()
            .filter_map(|(idx, received)| received.then_some(idx as u32))
            .take(maximum)
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

fn piece_state_memory_bytes(piece_length: u32) -> usize {
    let blocks = n_blocks(piece_length);
    std::mem::size_of::<PieceState>()
        .saturating_add(blocks.saturating_mul(std::mem::size_of::<u16>()))
        .saturating_add(blocks.saturating_mul(std::mem::size_of::<bool>()))
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
    in_progress_bytes: usize,
    /// Priority pieces (selected before rarest-first or sequential order).
    ///
    /// Keep this as packed flags rather than a `Vec<usize>`. A high-priority
    /// file can span every piece in a large torrent, and retaining one usize
    /// per piece would make a policy reload allocate hundreds of MiB.
    priority: Vec<bool>,
    sequential: bool,
    sequential_start_piece: usize,
    pub availability: Availability,
    /// Count of pieces where `wanted && enabled`, maintained incrementally
    /// so completion checks and the `pick`/`pick_endgame` fast path for a
    /// fully-seeded torrent are O(1) instead of a full scan. This is the
    /// common steady-state for a long-term seeder.
    outstanding_wanted: usize,
}

impl PiecePicker {
    /// Estimate the backing storage for the persistent per-torrent piece
    /// index. The three `Vec<bool>` fields are bit-packed, while availability
    /// stores one `u32` count per piece. Include allocator slack for the
    /// independent vectors and picker bookkeeping so the engine can reserve
    /// before constructing a large picker.
    pub fn memory_bytes_for_piece_count(piece_count: usize) -> usize {
        let bitset_bytes = piece_count
            .div_ceil(usize::BITS as usize)
            .saturating_mul(std::mem::size_of::<usize>());
        bitset_bytes
            .saturating_mul(3)
            .saturating_add(piece_count.saturating_mul(std::mem::size_of::<u32>()))
            .saturating_add(64 * 1024)
    }

    pub fn new(piece_count: usize, default_piece_length: u32, last_piece_length: u32) -> Self {
        PiecePicker {
            piece_count,
            default_piece_length,
            last_piece_length,
            wanted: vec![true; piece_count],
            enabled: vec![true; piece_count],
            in_progress: std::collections::HashMap::new(),
            in_progress_bytes: 0,
            priority: vec![false; piece_count],
            sequential: false,
            sequential_start_piece: 0,
            availability: Availability::new(piece_count),
            outstanding_wanted: piece_count,
        }
    }

    /// Update `outstanding_wanted` for a piece whose `wanted` and/or
    /// `enabled` flags just changed, and keep `availability`'s bucket walk
    /// in sync: retire the piece once it's no longer outstanding (so a
    /// long-lived download doesn't accumulate ever more completed pieces
    /// in a crowded bucket) or reinstate it if it becomes outstanding
    /// again. A piece counts as outstanding exactly when both flags are
    /// true, matching `is_complete`'s predicate.
    fn adjust_outstanding(
        &mut self,
        piece: usize,
        old_wanted: bool,
        old_enabled: bool,
        new_wanted: bool,
        new_enabled: bool,
    ) {
        let was_outstanding = old_wanted && old_enabled;
        let is_outstanding = new_wanted && new_enabled;
        if is_outstanding && !was_outstanding {
            self.outstanding_wanted = self.outstanding_wanted.saturating_add(1);
            self.availability.reinstate(piece);
        } else if was_outstanding && !is_outstanding {
            self.outstanding_wanted = self.outstanding_wanted.saturating_sub(1);
            self.availability.retire(piece);
        }
    }

    /// Mark a piece as already complete (from fastresume).
    pub fn mark_have(&mut self, piece: usize) {
        if piece < self.piece_count {
            let old_wanted = self.wanted[piece];
            let enabled = self.enabled[piece];
            self.wanted[piece] = false;
            self.adjust_outstanding(piece, old_wanted, enabled, false, enabled);
            self.remove_in_progress(piece);
        }
    }

    /// Mark a piece as needed again after verification failed.
    pub fn reject_piece(&mut self, piece: usize) {
        if piece < self.piece_count {
            let old_wanted = self.wanted[piece];
            let enabled = self.enabled[piece];
            self.wanted[piece] = true;
            self.adjust_outstanding(piece, old_wanted, enabled, true, enabled);
            self.remove_in_progress(piece);
        }
    }

    pub fn set_piece_enabled(&mut self, piece: usize, enabled: bool) {
        if piece < self.piece_count {
            let wanted = self.wanted[piece];
            let old_enabled = self.enabled[piece];
            self.enabled[piece] = enabled;
            self.adjust_outstanding(piece, wanted, old_enabled, wanted, enabled);
            if !enabled {
                self.remove_in_progress(piece);
            }
        }
    }

    pub fn restore_partial_piece(&mut self, piece: usize, received_blocks: &[u32]) {
        if piece >= self.piece_count
            || received_blocks.is_empty()
            || !self.wanted[piece]
            || !self.enabled[piece]
            || self.in_progress.contains_key(&piece)
            || !self.can_admit_piece_state(self.piece_length_for(piece))
        {
            return;
        }
        let piece_length = self.piece_length_for(piece);
        let state_bytes = piece_state_memory_bytes(piece_length);
        let mut state = PieceState::new(piece_length);
        for block_idx in received_blocks {
            if let Some(received) = state.received.get_mut(*block_idx as usize) {
                *received = true;
            }
        }
        // A partial-piece record contains bytes that were received but never
        // hash-verified. If it happens to list every block (for example after
        // a truncated or stale fastresume write), do not turn it into a
        // completed piece. The torrent actor must request the piece again and
        // verify it before advertising it as available.
        if !state.is_complete() {
            self.in_progress_bytes = self.in_progress_bytes.saturating_add(state_bytes);
            self.in_progress.insert(piece, state);
        }
    }

    /// Set priority pieces (head/tail of each file for fast preview).
    pub fn set_priority<I>(&mut self, pieces: I)
    where
        I: IntoIterator<Item = usize>,
    {
        let mut priority = vec![false; self.piece_count];
        for piece in pieces {
            if let Some(flag) = priority.get_mut(piece) {
                *flag = true;
            }
        }
        self.priority = priority;
    }

    /// Replace the packed priority flags without first expanding them into a
    /// list of piece indexes. The engine uses this when applying a durable
    /// file policy to a large torrent.
    pub fn set_priority_mask(&mut self, priority: Vec<bool>) {
        if priority.len() == self.piece_count {
            self.priority = priority;
        } else {
            self.priority.fill(false);
        }
    }

    /// Enable or disable sequential piece selection.
    pub fn set_sequential(&mut self, enabled: bool) {
        self.sequential = enabled;
    }

    /// Set the first piece considered by sequential mode.
    pub fn set_sequential_from_piece(&mut self, piece: usize) {
        self.sequential_start_piece = piece.min(self.piece_count.saturating_sub(1));
    }

    pub fn sequential(&self) -> bool {
        self.sequential
    }

    pub fn piece_count(&self) -> usize {
        self.piece_count
    }

    pub fn have_piece(&self, piece: usize) -> bool {
        self.wanted.get(piece).is_some_and(|wanted| !*wanted)
    }

    /// Pick the next block to request from a peer with given bitfield.
    ///
    /// Returns `None` if nothing is available from this peer right now.
    pub fn pick<A: PieceAvailability + ?Sized>(&mut self, peer_has: &A) -> Option<BlockRequest> {
        // Fast path: nothing left to request (e.g. a fully-seeded torrent,
        // the common steady state for a long-term seeder). Every branch
        // below requires a wanted+enabled piece, so this is exact, not an
        // approximation.
        if self.outstanding_wanted == 0 {
            return None;
        }

        // Priority pieces first.
        if let Some(piece) = (0..self.piece_count).find(|&piece| {
            self.priority[piece] && self.piece_is_requestable(piece) && peer_has.has_piece(piece)
        }) {
            return self.pick_block_from(piece);
        }

        if self.sequential {
            let start = self
                .sequential_start_piece
                .min(self.piece_count.saturating_sub(1));
            if let Some(piece) = (start..self.piece_count)
                .chain(0..start)
                .find(|&piece| self.piece_is_requestable(piece) && peer_has.has_piece(piece))
            {
                return self.pick_block_from(piece);
            }
            return None;
        }

        // Rarest-first among wanted pieces the peer has. See
        // `rarest_requestable_piece` for the bucket-walk this delegates to.
        let piece = self.rarest_requestable_piece(peer_has)?;
        self.pick_block_from(piece)
    }

    /// Pick from a source that is known to have every piece but is not counted
    /// in peer availability, such as a BEP19 webseed.
    pub fn pick_from_seed(&mut self) -> Option<BlockRequest> {
        if let Some(piece) = (0..self.piece_count)
            .find(|&piece| self.priority[piece] && self.piece_is_requestable(piece))
        {
            return self.pick_block_from(piece);
        }
        if self.sequential {
            let start = self
                .sequential_start_piece
                .min(self.piece_count.saturating_sub(1));
            if let Some(piece) = (start..self.piece_count)
                .chain(0..start)
                .find(|&piece| self.piece_is_requestable(piece))
            {
                return self.pick_block_from(piece);
            }
            return None;
        }
        for p in 0..self.piece_count {
            if self.piece_is_requestable(p) {
                return self.pick_block_from(p);
            }
        }
        None
    }

    /// Pick a duplicate outstanding block for endgame mode.
    ///
    /// This returns `Some` only after all enabled wanted pieces have no fresh
    /// unrequested blocks left. The caller supplies blocks already outstanding
    /// with the peer so we do not duplicate the same block to one peer.
    pub fn pick_endgame<A: PieceAvailability + ?Sized>(
        &mut self,
        peer_has: &A,
        already_requested_by_peer: &[BlockRequest],
    ) -> Option<BlockRequest> {
        if !self.endgame_active() {
            return None;
        }

        let piece = self.next_endgame_piece(peer_has, already_requested_by_peer)?;
        let state = self.in_progress.get_mut(&piece)?;
        let block_idx = state.next_requested_not_received(already_requested_by_peer, piece)?;
        state.mark_requested(block_idx);
        Some(state.block_request_for(piece, block_idx))
    }

    fn piece_is_requestable(&self, piece: usize) -> bool {
        piece < self.piece_count
            && self.wanted[piece]
            && self.enabled[piece]
            && self
                .in_progress
                .get(&piece)
                .is_none_or(|state| state.next_unrequested().is_some())
    }

    /// Rarest-first selection among wanted, requestable pieces the peer has.
    ///
    /// Walks the availability buckets from the rarest nonzero count upward
    /// instead of scanning every piece: O(buckets visited) amortized rather
    /// than O(piece_count) per call. This matters because `pick` is called
    /// once per outstanding block-request slot per peer (up to
    /// `PEER_REQUEST_PIPELINE_NORMAL` times per unchoke cycle).
    fn rarest_requestable_piece<A: PieceAvailability + ?Sized>(
        &self,
        peer_has: &A,
    ) -> Option<usize> {
        self.availability
            .rarest_matching(|piece| self.piece_is_requestable(piece) && peer_has.has_piece(piece))
    }

    fn next_endgame_piece<A: PieceAvailability + ?Sized>(
        &self,
        peer_has: &A,
        already_requested_by_peer: &[BlockRequest],
    ) -> Option<usize> {
        let is_candidate = |piece: usize| {
            piece < self.piece_count
                && self.wanted[piece]
                && self.enabled[piece]
                && peer_has.has_piece(piece)
                && self.in_progress.get(&piece).is_some_and(|state| {
                    state
                        .next_requested_not_received(already_requested_by_peer, piece)
                        .is_some()
                })
        };

        // Preserve the old order's priority-first behavior without cloning
        // the complete priority vector.
        if let Some(piece) =
            (0..self.piece_count).find(|&piece| self.priority[piece] && is_candidate(piece))
        {
            return Some(piece);
        }

        if self.sequential {
            let start = self
                .sequential_start_piece
                .min(self.piece_count.saturating_sub(1));
            return (start..self.piece_count)
                .chain(0..start)
                .find(|&piece| is_candidate(piece) && !self.priority[piece]);
        }

        // Same bucket-walk as `rarest_requestable_piece`; see its doc comment.
        self.availability
            .rarest_matching(|piece| !self.priority[piece] && is_candidate(piece))
    }

    fn endgame_active(&self) -> bool {
        if self.outstanding_wanted == 0 {
            return false;
        }
        self.wanted
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

    fn piece_length_for(&self, piece: usize) -> u32 {
        if piece + 1 == self.piece_count {
            self.last_piece_length
        } else {
            self.default_piece_length
        }
    }

    fn pick_block_from(&mut self, piece: usize) -> Option<BlockRequest> {
        if piece >= self.piece_count {
            return None;
        }
        let pl = self.piece_length_for(piece);
        let is_new = !self.in_progress.contains_key(&piece);
        if is_new && !self.can_admit_piece_state(pl) {
            return None;
        }
        if is_new {
            self.in_progress_bytes = self
                .in_progress_bytes
                .saturating_add(piece_state_memory_bytes(pl));
        }
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
                self.remove_in_progress(piece);
                let old_wanted = self.wanted[piece];
                let enabled = self.enabled[piece];
                self.wanted[piece] = false;
                self.adjust_outstanding(piece, old_wanted, enabled, false, enabled);
                return true;
            }
        }
        false
    }

    /// Return whether the picker still has partial state for a piece.
    ///
    /// Network paths can have duplicate endgame responses in flight. Once the
    /// first response completes a piece, later responses must not recreate an
    /// in-memory assembly or be counted as new progress.
    pub fn is_piece_in_progress(&self, piece: usize) -> bool {
        self.in_progress.contains_key(&piece)
    }

    /// Return whether a block has already been accepted for an in-progress
    /// piece. Endgame requests may be sent to multiple peers; a late duplicate
    /// must not overwrite the first response or turn a conflicting duplicate
    /// into a whole-piece rejection.
    pub fn is_block_received(&self, piece: usize, begin: u32) -> bool {
        let block_idx = (begin / MAX_BLOCK_SIZE) as usize;
        self.in_progress
            .get(&piece)
            .and_then(|state| state.received.get(block_idx))
            .copied()
            .unwrap_or(false)
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

    /// Drop all outstanding block request bookkeeping without discarding
    /// blocks already received for a partial piece.
    pub fn reset_outstanding_requests(&mut self) {
        for state in self.in_progress.values_mut() {
            state.requested.fill(0);
        }
    }

    pub fn is_complete(&self) -> bool {
        self.outstanding_wanted == 0
    }

    pub fn remaining_pieces(&self) -> usize {
        self.outstanding_wanted
    }

    pub fn bytes_left(&self) -> u64 {
        let mut left: u64 = self
            .wanted
            .iter()
            .enumerate()
            .filter(|&(piece, wanted)| *wanted && self.enabled[piece])
            .map(|(piece, _)| self.piece_length_for(piece) as u64)
            .sum();
        // Subtract bytes already received for in-progress pieces so progress
        // is visible before the first piece finishes hashing.
        for (&piece, state) in &self.in_progress {
            if piece < self.piece_count && self.wanted[piece] && self.enabled[piece] {
                let received_bytes = state
                    .received
                    .iter()
                    .enumerate()
                    .filter_map(|(block_idx, received)| {
                        received.then_some(u64::from(
                            state
                                .piece_length
                                .saturating_sub(block_idx as u32 * MAX_BLOCK_SIZE)
                                .min(MAX_BLOCK_SIZE),
                        ))
                    })
                    .fold(0_u64, u64::saturating_add);
                left = left.saturating_sub(received_bytes);
            }
        }
        left
    }

    pub fn have_pieces(&self) -> Vec<bool> {
        self.wanted.iter().map(|wanted| !*wanted).collect()
    }

    pub fn partial_pieces(&self) -> Vec<(u32, Vec<u32>)> {
        self.partial_pieces_limited(usize::MAX)
    }

    /// Snapshot received block hints with a per-piece allocation bound.
    ///
    /// Fastresume treats these indexes as an optimization, not as verified
    /// truth. Callers that persist them can therefore omit excess hints and
    /// let the torrent re-request those blocks after restart.
    pub fn partial_pieces_limited(&self, max_blocks_per_piece: usize) -> Vec<(u32, Vec<u32>)> {
        let mut partials: Vec<_> = self
            .in_progress
            .iter()
            .filter_map(|(piece, state)| {
                let blocks = state.received_blocks_limited(max_blocks_per_piece);
                (!blocks.is_empty()).then_some((*piece as u32, blocks))
            })
            .collect();
        partials.sort_by_key(|(piece, _)| *piece);
        partials
    }

    fn can_admit_piece_state(&self, piece_length: u32) -> bool {
        self.in_progress.len() < MAX_IN_PROGRESS_PIECES
            && self
                .in_progress_bytes
                .checked_add(piece_state_memory_bytes(piece_length))
                .is_some_and(|next| next <= MAX_IN_PROGRESS_PIECE_STATE_BYTES)
    }

    fn remove_in_progress(&mut self, piece: usize) {
        if let Some(state) = self.in_progress.remove(&piece) {
            self.in_progress_bytes = self
                .in_progress_bytes
                .saturating_sub(piece_state_memory_bytes(state.piece_length));
        }
    }
}

impl PieceAvailability for PiecePicker {
    fn has_piece(&self, piece: usize) -> bool {
        self.have_piece(piece)
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
    fn completed_piece_no_longer_accepts_late_blocks() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let request = p.pick(&all).unwrap();
        assert!(p.is_piece_in_progress(0));
        assert!(!p.is_block_received(0, request.begin));
        assert!(p.block_received(0, request.begin));
        assert!(!p.is_piece_in_progress(0));
        assert!(!p.is_block_received(0, request.begin));
    }

    #[test]
    fn received_block_is_detected_during_endgame() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let first = p.pick(&all).unwrap();
        let _second = p.pick(&all).unwrap();
        assert!(!p.block_received(0, first.begin));
        assert!(p.is_block_received(0, first.begin));
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
    fn mark_have_does_not_reenable_a_policy_disabled_piece() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        p.set_piece_enabled(0, false);

        p.mark_have(0);
        assert!(p.is_complete());
        assert_eq!(p.have_pieces(), vec![true]);

        p.reject_piece(0);
        assert!(p.is_complete());
        assert!(p.pick(&peer_has_all(1)).is_none());
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
    fn bytes_left_subtracts_partially_received_blocks() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 3 + 1);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let req = p.pick(&all).unwrap();
        let begin = req.begin;
        p.block_received(0, begin); // 1 block of ~16KB received
                                    // piece length = MAX_BLOCK_SIZE*3+1 = 49153, one block = 16384
        let expected = (MAX_BLOCK_SIZE * 3 + 1) as u64 - MAX_BLOCK_SIZE as u64;
        assert_eq!(p.bytes_left(), expected);
    }

    #[test]
    fn bytes_left_counts_a_short_final_block_exactly() {
        let piece_length = MAX_BLOCK_SIZE * 2 + 1;
        let mut p = picker_1piece(piece_length);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let first = p.pick(&all).unwrap();
        let second = p.pick(&all).unwrap();
        let tail = p.pick(&all).unwrap();
        assert_eq!(tail.length, 1);

        p.block_received(0, first.begin);
        p.block_received(0, second.begin);
        assert_eq!(p.bytes_left(), 1);

        p.block_received(0, tail.begin);
        assert_eq!(p.bytes_left(), 0);
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
    fn partial_piece_snapshot_can_bound_received_blocks() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 4);
        p.restore_partial_piece(0, &[0, 1, 2]);

        assert_eq!(p.partial_pieces_limited(2), vec![(0, vec![0, 1])]);
        assert_eq!(p.partial_pieces(), vec![(0, vec![0, 1, 2])]);
    }

    #[test]
    fn complete_partial_record_does_not_mark_piece_have() {
        let mut picker = picker_1piece(MAX_BLOCK_SIZE * 2);
        picker.availability.add_have(0);

        picker.restore_partial_piece(0, &[0, 1]);

        assert!(!picker.is_complete());
        assert_eq!(picker.partial_pieces(), Vec::<(u32, Vec<u32>)>::new());
        assert_eq!(picker.pick(&peer_has_all(1)).unwrap().begin, 0);
    }

    #[test]
    fn reinstated_piece_is_requestable_again_via_rarest_first() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let req = p.pick(&all).unwrap();
        assert!(p.block_received(0, req.begin));
        assert!(p.is_complete());
        // Completing the piece must retire it from the rarest-first bucket
        // walk, not just from `wanted`.
        assert!(p.pick(&all).is_none());

        p.reject_piece(0);
        assert!(!p.is_complete());
        // reject_piece must reinstate it into its bucket so rarest-first
        // finds it again, not just flip `wanted` back to true.
        let req2 = p.pick(&all).unwrap();
        assert_eq!(req2.piece, 0);
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
    fn reset_outstanding_requests_preserves_partial_piece_progress() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        p.availability.add_have(0);
        let all = peer_has_all(1);
        let first = p.pick(&all).unwrap();
        let second = p.pick(&all).unwrap();
        assert!(!p.block_received(0, first.begin));

        p.reset_outstanding_requests();

        let next = p.pick(&all).unwrap();
        assert_eq!(next.begin, second.begin);
        assert_eq!(p.partial_pieces(), vec![(0, vec![0])]);
    }

    #[test]
    fn seed_picker_does_not_require_peer_availability() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE * 2);
        let r1 = p.pick_from_seed().unwrap();
        let r2 = p.pick_from_seed().unwrap();
        assert_eq!(r1.begin, 0);
        assert_eq!(r2.begin, MAX_BLOCK_SIZE);
        assert!(p.pick_from_seed().is_none());
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
    fn invalid_priority_piece_is_ignored() {
        let mut p = picker_1piece(MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        p.set_priority(vec![usize::MAX, 0]);

        assert_eq!(p.pick(&peer_has_all(1)).unwrap().piece, 0);
    }

    #[test]
    fn sequential_mode_picks_lowest_piece_before_rarest() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        p.availability.add_have(0);
        p.availability.add_have(1);
        p.availability.add_have(1);
        p.availability.add_have(2);
        p.availability.add_have(2);
        p.availability.add_have(3);
        p.availability.add_have(3);
        p.set_sequential(true);

        let req = p.pick(&peer_has_all(4)).unwrap();

        assert_eq!(req.piece, 0);
    }

    #[test]
    fn sequential_from_piece_starts_at_configured_piece() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        p.set_sequential(true);
        p.set_sequential_from_piece(2);

        let req = p.pick(&peer_has_all(4)).unwrap();

        assert_eq!(req.piece, 2);
    }

    #[test]
    fn seed_picker_respects_sequential_from_piece() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        p.set_sequential(true);
        p.set_sequential_from_piece(3);

        let req = p.pick_from_seed().unwrap();

        assert_eq!(req.piece, 3);
    }

    #[test]
    fn endgame_pick_order_respects_sequential_from_piece() {
        let mut p = picker_4pieces(MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);
        p.set_sequential(true);
        p.set_sequential_from_piece(2);
        let all = peer_has_all(4);
        let requested: Vec<_> = (0..4).map(|_| p.pick(&all).unwrap()).collect();
        assert_eq!(
            requested.iter().map(|req| req.piece).collect::<Vec<_>>(),
            vec![2, 3, 0, 1]
        );

        let duplicate = p.pick_endgame(&all, &[]).unwrap();

        assert_eq!(duplicate.piece, 2);
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

    #[test]
    fn in_progress_piece_states_are_bounded() {
        let mut picker =
            PiecePicker::new(MAX_IN_PROGRESS_PIECES + 1, MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);

        for piece in 0..MAX_IN_PROGRESS_PIECES {
            assert!(picker.pick_block_from(piece).is_some());
        }

        assert!(picker.pick_block_from(MAX_IN_PROGRESS_PIECES).is_none());
        picker.restore_partial_piece(MAX_IN_PROGRESS_PIECES, &[0]);
        assert!(!picker.is_piece_in_progress(MAX_IN_PROGRESS_PIECES));
        assert_eq!(picker.in_progress.len(), MAX_IN_PROGRESS_PIECES);
    }

    #[test]
    fn in_progress_piece_state_bytes_are_bounded() {
        let piece_length = u32::MAX;
        let state_bytes = piece_state_memory_bytes(piece_length);
        let admitted = MAX_IN_PROGRESS_PIECE_STATE_BYTES / state_bytes;
        let mut picker = PiecePicker::new(admitted + 1, piece_length, piece_length);

        for piece in 0..admitted {
            picker.restore_partial_piece(piece, &[0]);
        }
        picker.restore_partial_piece(admitted, &[0]);

        assert_eq!(picker.in_progress.len(), admitted);
        assert!(picker.in_progress_bytes <= MAX_IN_PROGRESS_PIECE_STATE_BYTES);
        assert!(!picker.is_piece_in_progress(admitted));
    }

    #[test]
    fn piece_index_memory_estimate_accounts_for_dense_state() {
        let empty = PiecePicker::memory_bytes_for_piece_count(0);
        let one = PiecePicker::memory_bytes_for_piece_count(1);
        let many = PiecePicker::memory_bytes_for_piece_count(16_000_000);

        assert!(empty >= 64 * 1024);
        assert!(one >= empty);
        assert!(many > one);
        assert!(many >= 16_000_000 * std::mem::size_of::<u32>());
    }

    /// Large-scale correctness check for the availability-bucketed rarest
    /// picker (see `rarest_requestable_piece` / `Availability::rarest_matching`).
    ///
    /// Uses a piece count well past anything a linear-scan bug would pass
    /// unnoticed at small scale, a swarm with several overlapping,
    /// deterministic per-peer bitfields (so availability counts vary and
    /// buckets actually differ in size), and a downloading peer that has
    /// every piece so the rarest-first branch alone decides every pick.
    /// After each pick the block is immediately marked received, which
    /// exercises piece retirement (see `Availability::retire`) on every
    /// single iteration — the scenario where a naive bucket implementation
    /// would silently degrade back toward O(piece_count) as a download
    /// nears completion.
    ///
    /// Asserts the three ways bucket bookkeeping could go wrong: a piece
    /// never returned, a piece returned more than once while still
    /// in-flight, and a violation of the rarest-first ordering guarantee
    /// (availability counts of successive picks must be non-decreasing,
    /// since nothing else changes availability mid-run).
    #[test]
    fn rarest_first_bucket_walk_is_correct_at_scale() {
        const PIECE_COUNT: usize = 12_000;
        let mut p = PiecePicker::new(PIECE_COUNT, MAX_BLOCK_SIZE, MAX_BLOCK_SIZE);

        // Overlapping synthetic peers: peer 0 has every piece (guarantees
        // full coverage so every piece is eventually requestable), the
        // rest have deterministic sparser subsets so availability counts
        // vary across a handful of rarity buckets (1..=5).
        let moduli = [1usize, 2, 3, 5, 7];
        for &modulus in &moduli {
            for piece in (0..PIECE_COUNT).step_by(modulus) {
                p.availability.add_have(piece);
            }
        }

        let peer_has_everything = vec![true; PIECE_COUNT];

        let mut seen = std::collections::HashSet::with_capacity(PIECE_COUNT);
        let mut last_count = 0u32;
        for _ in 0..PIECE_COUNT {
            let req = p
                .pick(&peer_has_everything)
                .expect("every piece has availability >= 1 from peer 0's full bitfield");

            let piece = req.piece as usize;
            assert!(
                seen.insert(piece),
                "piece {piece} was returned more than once while outstanding"
            );

            let count = p.availability.count(piece);
            assert!(
                count >= last_count,
                "rarest-first ordering violated: piece {piece} (count {count}) picked after a piece with count {last_count}"
            );
            last_count = count;

            // Immediately complete the piece so the next pick has to walk
            // past an ever-growing set of retired pieces if retirement is
            // broken — this is the regression this test is meant to catch.
            assert!(p.block_received(piece, req.begin));
        }

        assert_eq!(seen.len(), PIECE_COUNT);
        assert!(p.is_complete());
        assert!(p.pick(&peer_has_everything).is_none());
    }
}
