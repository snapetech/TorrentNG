//! Persistent, immutable storage primitives used by API snapshots.
//!
//! A snapshot refresh usually changes a small number of torrents. Keeping the
//! item vector as one `Vec<T>` makes every refresh copy the complete pointer
//! array before it can replace those few items. `ChunkedVec` shares unchanged
//! chunks between generations and clones only the chunks containing updates.

use std::sync::Arc;

/// The unit of copy-on-write for an immutable API snapshot.
pub const SNAPSHOT_CHUNK_SIZE: usize = 256;
const BITSET_WORDS_PER_CHUNK: usize = 64;
const BITSET_BITS_PER_CHUNK: usize = BITSET_WORDS_PER_CHUNK * u64::BITS as usize;

#[derive(Debug, Clone)]
pub struct ChunkedVec<T> {
    chunks: Arc<Vec<Arc<Vec<T>>>>,
    len: usize,
}

impl<T> ChunkedVec<T> {
    pub fn from_vec(values: Vec<T>) -> Self
    where
        T: Clone,
    {
        let len = values.len();
        let chunks = values
            .chunks(SNAPSHOT_CHUNK_SIZE)
            .map(|chunk| Arc::new(chunk.to_vec()))
            .collect();
        Self {
            chunks: Arc::new(chunks),
            len,
        }
    }

    /// Replace existing positions with copy-on-write at chunk granularity.
    /// Callers must provide valid positions; invalid positions are ignored so
    /// a stale journal entry cannot panic a request path.
    pub fn replace_many<I>(&self, replacements: I) -> Self
    where
        T: Clone,
        I: IntoIterator<Item = (usize, T)>,
    {
        let mut chunks = self.chunks.as_ref().clone();
        for (index, value) in replacements {
            if index >= self.len {
                continue;
            }
            let chunk_index = index / SNAPSHOT_CHUNK_SIZE;
            let offset = index % SNAPSHOT_CHUNK_SIZE;
            if let Some(chunk) = chunks.get_mut(chunk_index) {
                Arc::make_mut(chunk)[offset] = value;
            }
        }
        Self {
            chunks: Arc::new(chunks),
            len: self.len,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        self.chunks
            .get(index / SNAPSHOT_CHUNK_SIZE)
            .and_then(|chunk| chunk.get(index % SNAPSHOT_CHUNK_SIZE))
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }

    pub fn range(&self, start: usize, end: usize) -> ChunkedRange<'_, T> {
        ChunkedRange {
            values: self,
            next: start.min(self.len),
            end: end.min(self.len),
        }
    }

    /// Structural changes (add/remove) are less common than field updates.
    /// They rebuild the compact representation and deliberately invalidate
    /// all positional indexes, which callers handle by rebuilding them.
    pub fn to_vec(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.iter().cloned().collect()
    }

    #[cfg(test)]
    pub fn chunk_ptr(&self, index: usize) -> Option<*const Vec<T>> {
        self.chunks.get(index).map(Arc::as_ptr)
    }
}

pub struct ChunkedRange<'a, T> {
    values: &'a ChunkedVec<T>,
    next: usize,
    end: usize,
}

impl<'a, T> Iterator for ChunkedRange<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let item = self.values.get(self.next);
        self.next += 1;
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for ChunkedRange<'a, T> {}

/// A copy-on-write bitmap for positional snapshot indexes. Membership changes
/// clone one bitmap chunk instead of the complete list of matching positions.
#[derive(Debug, Clone)]
pub struct ChunkedBitSet {
    chunks: Arc<Vec<Arc<Vec<u64>>>>,
    len: usize,
}

impl ChunkedBitSet {
    pub fn empty(len: usize) -> Self {
        let chunk_count = len.div_ceil(BITSET_BITS_PER_CHUNK);
        let chunks = (0..chunk_count)
            .map(|_| Arc::new(vec![0; BITSET_WORDS_PER_CHUNK]))
            .collect();
        Self {
            chunks: Arc::new(chunks),
            len,
        }
    }

    pub fn from_indices(len: usize, indices: impl IntoIterator<Item = usize>) -> Self {
        let chunk_count = len.div_ceil(BITSET_BITS_PER_CHUNK);
        let mut chunks = vec![vec![0_u64; BITSET_WORDS_PER_CHUNK]; chunk_count];
        for index in indices {
            if index >= len {
                continue;
            }
            let chunk_index = index / BITSET_BITS_PER_CHUNK;
            let word_index = (index % BITSET_BITS_PER_CHUNK) / u64::BITS as usize;
            let bit = (index % u64::BITS as usize) as u32;
            chunks[chunk_index][word_index] |= 1_u64 << bit;
        }
        Self {
            chunks: Arc::new(chunks.into_iter().map(Arc::new).collect()),
            len,
        }
    }

    pub fn set(&self, index: usize, present: bool) -> Self {
        if index >= self.len {
            return self.clone();
        }
        let chunk_index = index / BITSET_BITS_PER_CHUNK;
        let word_index = (index % BITSET_BITS_PER_CHUNK) / u64::BITS as usize;
        let bit = (index % u64::BITS as usize) as u32;
        let mut chunks = self.chunks.as_ref().clone();
        if let Some(chunk) = chunks.get_mut(chunk_index) {
            let word = &mut Arc::make_mut(chunk)[word_index];
            if present {
                *word |= 1_u64 << bit;
            } else {
                *word &= !(1_u64 << bit);
            }
        }
        Self {
            chunks: Arc::new(chunks),
            len: self.len,
        }
    }

    pub fn contains(&self, index: usize) -> bool {
        if index >= self.len {
            return false;
        }
        let chunk = &self.chunks[index / BITSET_BITS_PER_CHUNK];
        let word = chunk[(index % BITSET_BITS_PER_CHUNK) / u64::BITS as usize];
        (word & (1_u64 << (index % u64::BITS as usize))) != 0
    }

    pub fn indices(&self) -> Vec<usize> {
        let mut result = Vec::new();
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            for (word_index, word) in chunk.iter().copied().enumerate() {
                let mut remaining = word;
                while remaining != 0 {
                    let bit = remaining.trailing_zeros() as usize;
                    let index =
                        chunk_index * BITSET_BITS_PER_CHUNK + word_index * u64::BITS as usize + bit;
                    if index < self.len {
                        result.push(index);
                    }
                    remaining &= remaining - 1;
                }
            }
        }
        result
    }

    /// Return the number of set positions without materializing them.
    ///
    /// Facet/count endpoints use this for immutable snapshot indexes so a
    /// category or tag count does not allocate a position vector just to
    /// report its cardinality.
    pub fn count(&self) -> usize {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// Return the first set position without materializing the full index.
    pub fn first_index(&self) -> Option<usize> {
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            for (word_index, word) in chunk.iter().copied().enumerate() {
                if word != 0 {
                    let bit = word.trailing_zeros() as usize;
                    let index =
                        chunk_index * BITSET_BITS_PER_CHUNK + word_index * u64::BITS as usize + bit;
                    return (index < self.len).then_some(index);
                }
            }
        }
        None
    }

    #[cfg(test)]
    fn chunk_ptr(&self, index: usize) -> Option<*const Vec<u64>> {
        self.chunks.get(index).map(Arc::as_ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_one_item_shares_unmodified_chunks() {
        let values = (0..(SNAPSHOT_CHUNK_SIZE * 2 + 1)).collect::<Vec<_>>();
        let first = ChunkedVec::from_vec(values);
        let second = first.replace_many([(SNAPSHOT_CHUNK_SIZE + 1, 99)]);

        assert_eq!(second.len(), first.len());
        assert_eq!(second.get(SNAPSHOT_CHUNK_SIZE + 1), Some(&99));
        assert_eq!(
            first.get(SNAPSHOT_CHUNK_SIZE + 1),
            Some(&(SNAPSHOT_CHUNK_SIZE + 1))
        );
        assert_eq!(first.chunk_ptr(0), second.chunk_ptr(0));
        assert_ne!(first.chunk_ptr(1), second.chunk_ptr(1));
        assert_eq!(first.chunk_ptr(2), second.chunk_ptr(2));
    }

    #[test]
    fn range_is_bounded_and_exact() {
        let values = ChunkedVec::from_vec((0..10).collect::<Vec<_>>());
        assert_eq!(
            values.range(3, 7).copied().collect::<Vec<_>>(),
            vec![3, 4, 5, 6]
        );
        assert_eq!(values.range(8, 100).len(), 2);
    }

    #[test]
    fn bitmap_membership_updates_share_unmodified_chunks() {
        let first = ChunkedBitSet::from_indices(10_000, [1, 8_000]);
        let second = first.set(8_001, true);
        assert!(second.contains(8_001));
        assert!(!first.contains(8_001));
        assert_eq!(first.count(), 2);
        assert_eq!(first.first_index(), Some(1));
        assert_eq!(ChunkedBitSet::empty(10_000).first_index(), None);
        assert_eq!(first.chunk_ptr(0), second.chunk_ptr(0));
        assert_ne!(first.chunk_ptr(1), second.chunk_ptr(1));
        assert_eq!(second.indices(), vec![1, 8_000, 8_001]);
    }
}
