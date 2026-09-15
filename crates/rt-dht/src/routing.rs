/// BEP 5 routing table: 160 k-buckets, each holding up to K=8 nodes.
use std::net::{SocketAddrV4, SocketAddrV6};

use crate::node_id::NodeId;

pub const K: usize = 8;
pub const BUCKET_COUNT: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KNode {
    pub id: NodeId,
    pub addr: SocketAddrV4,
}

/// IPv6 endpoint stored in the independent BEP 32 routing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KNode6 {
    pub id: NodeId,
    pub addr: SocketAddrV6,
}

#[derive(Debug, Default, Clone)]
pub struct KBucket {
    nodes: Vec<KNode>,
}

impl KBucket {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.nodes.len() >= K
    }

    /// Try to insert a node. Returns true if inserted.
    /// Does not insert duplicates (by node ID) or if bucket is full.
    ///
    /// A repeated ID is still useful: DHT nodes can change their endpoint.
    /// Refresh the stored address so future lookups do not keep dialing a
    /// stale endpoint. The return value remains `false` because no new node
    /// was inserted.
    pub fn insert(&mut self, node: KNode) -> bool {
        if let Some(existing) = self.nodes.iter_mut().find(|n| n.id == node.id) {
            existing.addr = node.addr;
            return false;
        }
        if self.is_full() {
            return false;
        }
        self.nodes.push(node);
        true
    }

    pub fn remove(&mut self, id: &NodeId) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| &n.id != id);
        self.nodes.len() < before
    }

    pub fn nodes(&self) -> &[KNode] {
        &self.nodes
    }

    /// Closest K nodes to `target` from this bucket.
    pub fn closest_to(&self, target: &NodeId, k: usize) -> Vec<&KNode> {
        let mut with_dist: Vec<(&KNode, _)> = self
            .nodes
            .iter()
            .map(|n| (n, n.id.distance(target)))
            .collect();
        with_dist.sort_unstable_by_key(|(_, d)| *d);
        with_dist.into_iter().take(k).map(|(n, _)| n).collect()
    }
}

/// Full routing table keyed on 160 buckets.
pub struct RoutingTable {
    pub local_id: NodeId,
    buckets: Vec<KBucket>,
}

impl RoutingTable {
    pub fn new(local_id: NodeId) -> Self {
        RoutingTable {
            local_id,
            buckets: vec![KBucket::default(); BUCKET_COUNT],
        }
    }

    fn bucket_idx(&self, id: &NodeId) -> usize {
        let dist = self.local_id.distance(id);
        dist.bucket_index().unwrap_or(0)
    }

    pub fn insert(&mut self, node: KNode) -> bool {
        if node.id == self.local_id {
            return false;
        }
        let idx = self.bucket_idx(&node.id);
        self.buckets[idx].insert(node)
    }

    pub fn remove(&mut self, id: &NodeId) -> bool {
        let idx = self.bucket_idx(id);
        self.buckets[idx].remove(id)
    }

    /// Find the K nodes closest to `target`.
    pub fn closest(&self, target: &NodeId, k: usize) -> Vec<&KNode> {
        let mut candidates: Vec<(&KNode, _)> = self
            .buckets
            .iter()
            .flat_map(|b| b.nodes().iter())
            .map(|n| (n, n.id.distance(target)))
            .collect();
        candidates.sort_unstable_by_key(|(_, d)| *d);
        candidates.into_iter().take(k).map(|(n, _)| n).collect()
    }

    pub fn total_nodes(&self) -> usize {
        self.buckets.iter().map(|b| b.len()).sum()
    }
}

#[derive(Debug, Default, Clone)]
pub struct KBucket6 {
    nodes: Vec<KNode6>,
}

impl KBucket6 {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.nodes.len() >= K
    }

    pub fn insert(&mut self, node: KNode6) -> bool {
        if let Some(existing) = self.nodes.iter_mut().find(|n| n.id == node.id) {
            existing.addr = node.addr;
            return false;
        }
        if self.is_full() {
            return false;
        }
        self.nodes.push(node);
        true
    }

    pub fn remove(&mut self, id: &NodeId) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|node| &node.id != id);
        self.nodes.len() < before
    }

    pub fn nodes(&self) -> &[KNode6] {
        &self.nodes
    }
}

/// Independent IPv6 BEP 32 routing table. IPv4 and IPv6 tables intentionally
/// do not share buckets: endpoint-family separation is part of the protocol.
pub struct RoutingTable6 {
    pub local_id: NodeId,
    buckets: Vec<KBucket6>,
}

impl RoutingTable6 {
    pub fn new(local_id: NodeId) -> Self {
        Self {
            local_id,
            buckets: vec![KBucket6::default(); BUCKET_COUNT],
        }
    }

    fn bucket_idx(&self, id: &NodeId) -> usize {
        self.local_id.distance(id).bucket_index().unwrap_or(0)
    }

    pub fn insert(&mut self, node: KNode6) -> bool {
        if node.id == self.local_id {
            return false;
        }
        let idx = self.bucket_idx(&node.id);
        self.buckets[idx].insert(node)
    }

    pub fn remove(&mut self, id: &NodeId) -> bool {
        let idx = self.bucket_idx(id);
        self.buckets[idx].remove(id)
    }

    pub fn closest(&self, target: &NodeId, k: usize) -> Vec<&KNode6> {
        let mut candidates: Vec<(&KNode6, _)> = self
            .buckets
            .iter()
            .flat_map(|bucket| bucket.nodes().iter())
            .map(|node| (node, node.id.distance(target)))
            .collect();
        candidates.sort_unstable_by_key(|(_, distance)| *distance);
        candidates
            .into_iter()
            .take(k)
            .map(|(node, _)| node)
            .collect()
    }

    pub fn total_nodes(&self) -> usize {
        self.buckets.iter().map(KBucket6::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: [u8; 20]) -> KNode {
        KNode {
            id: NodeId::from_bytes(id),
            addr: "127.0.0.1:6881".parse().unwrap(),
        }
    }

    #[test]
    fn insert_and_find() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        let mut nid = [0u8; 20];
        nid[19] = 1;
        assert!(table.insert(make_node(nid)));
        assert_eq!(table.total_nodes(), 1);
    }

    #[test]
    fn self_not_inserted() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        assert!(!table.insert(make_node([0u8; 20])));
        assert_eq!(table.total_nodes(), 0);
    }

    #[test]
    fn bucket_capacity() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        // Insert K+1 nodes into the same bucket (all share bit 7 as highest XOR bit)
        for i in 0u8..=(K as u8) {
            let mut nid = [0u8; 20];
            nid[19] = 0x80 | i; // bit 7 always highest → same bucket
            table.insert(make_node(nid));
        }
        assert_eq!(table.total_nodes(), K); // capped at K
    }

    #[test]
    fn closest_returns_k_nearest() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        for i in 1u8..=16 {
            let mut nid = [0xFF; 20];
            nid[19] = i;
            table.insert(make_node(nid));
        }
        let target = NodeId::from_bytes([0xFFu8; 20]);
        let closest = table.closest(&target, K);
        assert!(closest.len() <= K);
    }

    #[test]
    fn remove_node() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        let mut nid = [0u8; 20];
        nid[19] = 5;
        table.insert(make_node(nid));
        let id = NodeId::from_bytes(nid);
        assert!(table.remove(&id));
        assert_eq!(table.total_nodes(), 0);
    }

    #[test]
    fn duplicate_node_id_refreshes_endpoint() {
        let local = NodeId::from_bytes([0u8; 20]);
        let mut table = RoutingTable::new(local);
        let id = NodeId::from_bytes({
            let mut bytes = [0u8; 20];
            bytes[19] = 1;
            bytes
        });

        assert!(table.insert(KNode {
            id,
            addr: "127.0.0.1:6881".parse().unwrap(),
        }));
        assert!(!table.insert(KNode {
            id,
            addr: "127.0.0.2:6882".parse().unwrap(),
        }));

        let closest = table.closest(&id, 1);
        assert_eq!(closest[0].addr, "127.0.0.2:6882".parse().unwrap());
        assert_eq!(table.total_nodes(), 1);
    }
}
