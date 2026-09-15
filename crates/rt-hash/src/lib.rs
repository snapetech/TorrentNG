/// BEP 52 v2 hash primitives: SHA-256 block hashes and v2 infohash.
use sha2::{Digest, Sha256};

const MERKLE_MAX_LEVELS: usize = usize::BITS as usize + 1;

/// BEP 52 hashes file data in 16 KiB blocks.
pub const V2_BLOCK_SIZE: usize = 16 * 1024;

/// SHA-256 hash of a 16 KiB leaf block (or the last block of a file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockHash(pub [u8; 32]);

impl BlockHash {
    pub fn of(data: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(data);
        BlockHash(h.finalize().into())
    }
}

/// SHA-256 of a pair of already-computed merkle children.
pub fn hash_pair(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Compute the merkle root of a sequence of leaf hashes by pairing up levels.
/// Pads with zero hashes when the count is not a power of two.
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let n = leaves.len().next_power_of_two();
    let mut level: Vec<[u8; 32]> = Vec::with_capacity(n);
    level.extend_from_slice(leaves);
    while level.len() < n {
        level.push([0u8; 32]);
    }
    while level.len() > 1 {
        let next_len = level.len() / 2;
        let mut next = Vec::with_capacity(next_len);
        for pair in level.as_chunks::<2>().0 {
            next.push(hash_pair(pair[0], pair[1]));
        }
        level = next;
    }
    level[0]
}

/// Return every level of a padded merkle tree, starting with the leaf level.
/// The first level is padded with zero hashes to a power of two, exactly as
/// required by BEP 52's file roots and piece layers.
pub fn merkle_layers(leaves: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
    if leaves.is_empty() {
        return vec![vec![[0u8; 32]]];
    }
    let n = leaves.len().next_power_of_two();
    let mut levels = Vec::new();
    let mut level = Vec::with_capacity(n);
    level.extend_from_slice(leaves);
    level.resize(n, [0u8; 32]);
    levels.push(level);
    while levels.last().is_some_and(|level| level.len() > 1) {
        let previous = levels.last().expect("merkle level exists");
        let next = previous
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| hash_pair(pair[0], pair[1]))
            .collect();
        levels.push(next);
    }
    levels
}

/// Hash one BEP 52 piece-sized data segment into its piece-layer hash. The
/// segment is padded with zero hash children up to `piece_length / 16 KiB`.
/// `piece_length` must be a power of two and at least one block.
pub fn piece_layer_hash(data: &[u8], piece_length: usize) -> Option<[u8; 32]> {
    if piece_length < V2_BLOCK_SIZE
        || !piece_length.is_power_of_two()
        || !piece_length.is_multiple_of(V2_BLOCK_SIZE)
        || data.len() > piece_length
    {
        return None;
    }
    let block_count = piece_length / V2_BLOCK_SIZE;
    let leaf_count = data.len().div_ceil(V2_BLOCK_SIZE).max(1);
    let mut leaves = Vec::with_capacity(block_count);
    for block in data.chunks(V2_BLOCK_SIZE) {
        leaves.push(BlockHash::of(block).0);
    }
    // A short final block is hashed as-is; unused blocks are zero hash nodes,
    // not SHA-256 hashes of empty byte strings.
    leaves.resize(block_count.max(leaf_count), [0u8; 32]);
    Some(merkle_root(&leaves))
}

/// Incrementally computes a padded v2 merkle root without retaining every
/// leaf hash. The accumulator stores at most one subtree per tree level.
#[derive(Debug)]
pub struct MerkleAccumulator {
    levels: Vec<Option<[u8; 32]>>,
    leaf_count: usize,
}

impl MerkleAccumulator {
    pub fn new() -> Self {
        Self {
            levels: vec![None; MERKLE_MAX_LEVELS],
            leaf_count: 0,
        }
    }

    /// Add one leaf in content order.
    pub fn push(&mut self, leaf: [u8; 32]) {
        self.leaf_count = self
            .leaf_count
            .checked_add(1)
            .expect("merkle accumulator leaf count overflow");
        self.push_node(leaf, 0);
    }

    /// Finish the root, padding the leaf sequence with zero hashes to the
    /// next power of two, matching [`merkle_root`].
    pub fn finish(mut self) -> [u8; 32] {
        if self.leaf_count == 0 {
            return [0u8; 32];
        }

        let target = self
            .leaf_count
            .checked_next_power_of_two()
            .expect("merkle accumulator target size overflow");
        let mut padding = target - self.leaf_count;
        let mut level = 0usize;
        while padding != 0 {
            if padding & 1 == 1 {
                self.push_node(zero_subtree_hash(level), level);
            }
            padding >>= 1;
            level += 1;
        }

        self.levels
            .into_iter()
            .flatten()
            .next()
            .expect("merkle accumulator did not produce a root")
    }

    fn push_node(&mut self, mut node: [u8; 32], mut level: usize) {
        loop {
            let existing = self
                .levels
                .get_mut(level)
                .expect("merkle accumulator level overflow")
                .take();
            match existing {
                Some(left) => {
                    node = hash_pair(left, node);
                    level += 1;
                }
                None => {
                    self.levels[level] = Some(node);
                    return;
                }
            }
        }
    }
}

impl Default for MerkleAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

fn zero_subtree_hash(level: usize) -> [u8; 32] {
    let mut hash = [0u8; 32];
    for _ in 0..level {
        hash = hash_pair(hash, hash);
    }
    hash
}

/// v2 infohash: SHA-256 of the bencoded info dict (BEP 52).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InfoHashV2(pub [u8; 32]);

impl InfoHashV2 {
    pub fn of_info_bytes(info_bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(info_bytes);
        InfoHashV2(h.finalize().into())
    }

    /// Truncated 20-byte form for backwards-compat display and tracker announces.
    pub fn truncated(&self) -> [u8; 20] {
        self.0[..20].try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_hash_deterministic() {
        let a = BlockHash::of(b"hello");
        let b = BlockHash::of(b"hello");
        assert_eq!(a, b);
        let c = BlockHash::of(b"world");
        assert_ne!(a, c);
    }

    #[test]
    fn merkle_root_single_leaf() {
        let leaf = BlockHash::of(b"data");
        let root = merkle_root(&[leaf.0]);
        assert_eq!(root, leaf.0);
    }

    #[test]
    fn merkle_root_two_leaves() {
        let a = BlockHash::of(b"a").0;
        let b = BlockHash::of(b"b").0;
        let root = merkle_root(&[a, b]);
        let mut h = Sha256::new();
        h.update(a);
        h.update(b);
        let expected: [u8; 32] = h.finalize().into();
        assert_eq!(root, expected);
    }

    #[test]
    fn merkle_root_pads_to_power_of_two() {
        let leaves: Vec<[u8; 32]> = (0u8..3).map(|i| BlockHash::of(&[i]).0).collect();
        let root = merkle_root(&leaves);
        assert_ne!(root, [0u8; 32]);
    }

    #[test]
    fn merkle_accumulator_matches_batch_root() {
        for leaf_count in 0..=65 {
            let leaves: Vec<[u8; 32]> = (0..leaf_count)
                .map(|index| BlockHash::of(&[(index % 251) as u8]).0)
                .collect();
            let mut accumulator = MerkleAccumulator::new();
            for &leaf in &leaves {
                accumulator.push(leaf);
            }
            assert_eq!(
                accumulator.finish(),
                merkle_root(&leaves),
                "leaf_count={leaf_count}"
            );
        }
    }

    #[test]
    fn infohash_v2_truncated_length() {
        let h = InfoHashV2::of_info_bytes(b"some info dict bytes");
        assert_eq!(h.truncated().len(), 20);
    }
}
