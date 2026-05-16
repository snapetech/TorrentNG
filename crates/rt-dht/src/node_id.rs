/// BEP 5: 160-bit DHT node ID with XOR-distance metric.
use rand::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId([u8; 20]);

impl NodeId {
    pub fn random() -> Self {
        let mut id = [0u8; 20];
        rand::thread_rng().fill(&mut id);
        NodeId(id)
    }

    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        NodeId(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// XOR distance between two node IDs (BEP 5 metric).
    pub fn distance(&self, other: &NodeId) -> Distance {
        let mut d = [0u8; 20];
        for (i, (a, b)) in self.0.iter().zip(other.0.iter()).enumerate() {
            d[i] = a ^ b;
        }
        Distance(d)
    }

    /// Index of the highest set bit (0 = LSB of last byte).
    /// Used to determine which routing table bucket a node belongs to.
    pub fn leading_zeros(&self) -> u32 {
        for (i, &byte) in self.0.iter().enumerate() {
            if byte != 0 {
                return i as u32 * 8 + byte.leading_zeros();
            }
        }
        160
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// XOR distance value (used for comparison).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Distance([u8; 20]);

impl Distance {
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }

    /// Which bucket (0-159) this distance falls into (index of highest set bit).
    pub fn bucket_index(&self) -> Option<usize> {
        for (i, &byte) in self.0.iter().enumerate() {
            if byte != 0 {
                return Some(i * 8 + byte.leading_zeros() as usize);
            }
        }
        None // distance is zero — same node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_to_self_is_zero() {
        let id = NodeId::random();
        assert!(id.distance(&id).is_zero());
    }

    #[test]
    fn distance_xor_symmetric() {
        let a = NodeId::random();
        let b = NodeId::random();
        assert_eq!(a.distance(&b), b.distance(&a));
    }

    #[test]
    fn known_distance() {
        let a = NodeId::from_bytes([0u8; 20]);
        let mut b_bytes = [0u8; 20];
        b_bytes[0] = 0x80; // highest bit set
        let b = NodeId::from_bytes(b_bytes);
        let d = a.distance(&b);
        assert_eq!(d.0[0], 0x80);
        assert_eq!(d.bucket_index(), Some(0));
    }

    #[test]
    fn leading_zeros_all_zero() {
        let id = NodeId::from_bytes([0u8; 20]);
        assert_eq!(id.leading_zeros(), 160);
    }

    #[test]
    fn display_is_40_hex_chars() {
        let id = NodeId::random();
        assert_eq!(id.to_string().len(), 40);
    }
}
