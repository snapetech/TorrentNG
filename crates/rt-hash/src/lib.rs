/// BEP 52 v2 hash primitives: SHA-256 block hashes and v2 infohash.
use sha2::{Digest, Sha256};

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
        for pair in level.chunks_exact(2) {
            let mut h = Sha256::new();
            h.update(pair[0]);
            h.update(pair[1]);
            next.push(h.finalize().into());
        }
        level = next;
    }
    level[0]
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
    fn infohash_v2_truncated_length() {
        let h = InfoHashV2::of_info_bytes(b"some info dict bytes");
        assert_eq!(h.truncated().len(), 20);
    }
}
