//! Minimal BEP 5 DHT service loop.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::{Duration, Instant};

use anyhow::Context;
use rt_dht::{DhtError, DhtQuery, DhtResponse, KNode, KrpcMessage, NodeId, RoutingTable, K};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::torrent_task::TorrentCmd;

const DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP: usize = 512;

#[derive(Clone)]
pub struct DhtTorrent {
    pub info_hash: [u8; 20],
    pub cmd_tx: mpsc::Sender<TorrentCmd>,
}

pub enum DhtCommand {
    AddTorrent(DhtTorrent),
    RemoveTorrent([u8; 20]),
    GetStats {
        reply: oneshot::Sender<DhtRuntimeStats>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DhtRuntimeStats {
    pub routing_nodes: u64,
    pub announced_peer_sets: u64,
    pub announced_peers: u64,
    pub tracked_torrents: u64,
    pub outstanding_requests: u64,
    pub queried_nodes: u64,
}

pub async fn run_dht(
    port: u16,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    mut cmd_rx: mpsc::Receiver<DhtCommand>,
) -> anyhow::Result<()> {
    let local_id = NodeId::random();
    let socket = UdpSocket::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("binding DHT UDP port {port}"))?;
    let bound = socket.local_addr()?;
    info!(
        component = "dht",
        operation = "listen",
        addr = %bound,
        node_id = %local_id,
        "DHT UDP socket bound"
    );

    // TNG-019: previously always started at 1 and incremented sequentially,
    // meaning transaction ids were fully predictable across every daemon
    // restart (not just within one running session). Combined with the
    // response source-address check above, an attacker guessing this
    // sequence is no longer enough on its own to inject a forged response,
    // but starting from a random point still removes a cheap, free signal.
    // Reuses `NodeId::random()` (already backed by `rand` inside rt-dht)
    // instead of adding a new direct dependency just for two bytes.
    let seed_bytes = *rt_dht::NodeId::random().as_bytes();
    // `.max(1)` matches `transaction_id()`'s own invariant that a
    // transaction id is never the all-zero value.
    let random_tx_seed = u16::from_be_bytes([seed_bytes[0], seed_bytes[1]]).max(1);
    let mut task = DhtTask {
        local_id,
        table: RoutingTable::new(local_id),
        socket,
        listen_port,
        bootstrap_nodes,
        next_tx: random_tx_seed,
        outstanding: HashMap::new(),
        queried_nodes: HashMap::new(),
        torrents: HashMap::new(),
        announced_peers: HashMap::new(),
        last_full_lookup: HashMap::new(),
    };
    task.bootstrap().await;

    let mut bootstrap_tick = interval(Duration::from_secs(300));
    let mut search_tick = interval(Duration::from_secs(30));
    let mut outstanding_sweep_tick = interval(Duration::from_secs(10));
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            Some(cmd) = cmd_rx.recv() => {
                if !task.handle_command(cmd).await {
                    break;
                }
            }
            _ = bootstrap_tick.tick() => {
                if task.table.total_nodes() < K {
                    task.bootstrap().await;
                }
            }
            _ = search_tick.tick() => {
                task.search_torrents().await;
            }
            _ = outstanding_sweep_tick.tick() => {
                task.prune_stale_outstanding();
            }
            recv = task.socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, addr)) => task.handle_packet(&buf[..n], addr).await,
                    Err(e) => warn!(
                        component = "dht",
                        operation = "recv",
                        result = "error",
                        error = %e,
                        "DHT UDP receive failed"
                    ),
                }
            }
        }
    }
    Ok(())
}

struct DhtTask {
    local_id: NodeId,
    table: RoutingTable,
    socket: UdpSocket,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    next_tx: u16,
    outstanding: HashMap<Vec<u8>, OutstandingQuery>,
    queried_nodes: HashMap<[u8; 20], HashSet<SocketAddrV4>>,
    torrents: HashMap<[u8; 20], mpsc::Sender<TorrentCmd>>,
    announced_peers: HashMap<[u8; 20], Vec<SocketAddr>>,
    last_full_lookup: HashMap<[u8; 20], Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DhtRequest {
    Bootstrap,
    GetPeers([u8; 20]),
    AnnouncePeer,
}

/// TNG-019: a query we sent and are waiting on a response to. Records the
/// address we actually sent it to (`addr`) so an incoming `Response`/`Error`
/// claiming a matching transaction ID can be checked against it -- without
/// this, any UDP packet from *any* source that happens to guess or replay a
/// valid transaction ID would be accepted as if it came from the queried
/// node, letting an off-path attacker inject forged nodes/peers into the
/// routing table or a get_peers result. Also records `sent_at` so stale
/// entries (queried nodes that never answered) can be pruned instead of
/// growing `outstanding` unboundedly.
#[derive(Debug, Clone, Copy)]
struct OutstandingQuery {
    addr: SocketAddr,
    request: DhtRequest,
    sent_at: Instant,
}

/// Outstanding queries older than this never got a response and are
/// considered abandoned -- pruned so `outstanding` doesn't grow forever
/// from nodes that silently drop our packets.
const OUTSTANDING_QUERY_TTL: Duration = Duration::from_secs(30);

impl DhtTask {
    async fn handle_command(&mut self, cmd: DhtCommand) -> bool {
        match cmd {
            DhtCommand::AddTorrent(torrent) => {
                self.torrents.insert(torrent.info_hash, torrent.cmd_tx);
                self.search_torrent(torrent.info_hash, true).await;
            }
            DhtCommand::RemoveTorrent(info_hash) => {
                self.torrents.remove(&info_hash);
                self.queried_nodes.remove(&info_hash);
                self.announced_peers.remove(&info_hash);
                self.last_full_lookup.remove(&info_hash);
            }
            DhtCommand::GetStats { reply } => {
                let _ = reply.send(self.runtime_stats());
            }
            DhtCommand::Shutdown { reply } => {
                info!(
                    component = "dht",
                    operation = "shutdown",
                    result = "ok",
                    "DHT task shutting down"
                );
                let _ = reply.send(());
                return false;
            }
        }
        true
    }

    async fn bootstrap(&mut self) {
        for node in self.bootstrap_nodes.clone() {
            let addrs = match tokio::net::lookup_host(&node).await {
                Ok(addrs) => addrs,
                Err(e) => {
                    warn!(
                        component = "dht",
                        operation = "bootstrap_resolve",
                        node = %node,
                        result = "error",
                        error = %e,
                        "DHT bootstrap resolve failed"
                    );
                    continue;
                }
            };
            for addr in addrs {
                if !addr.is_ipv4() {
                    continue;
                }
                let tx = self.transaction_id();
                let msg = KrpcMessage::Query {
                    transaction_id: tx.clone(),
                    query: DhtQuery::FindNode {
                        id: self.local_id,
                        target: self.local_id,
                    },
                };
                self.outstanding.insert(
                    tx,
                    OutstandingQuery {
                        addr,
                        request: DhtRequest::Bootstrap,
                        sent_at: Instant::now(),
                    },
                );
                if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
                    warn!(
                        component = "dht",
                        operation = "bootstrap_query",
                        node = %addr,
                        result = "error",
                        error = %e,
                        "DHT bootstrap query send failed"
                    );
                }
            }
        }
    }

    async fn handle_packet(&mut self, packet: &[u8], addr: SocketAddr) {
        let msg = match KrpcMessage::parse(packet) {
            Ok(msg) => msg,
            Err(e) => {
                debug!(
                    component = "dht",
                    operation = "parse_packet",
                    peer = %addr,
                    result = "error",
                    error = %e,
                    "invalid DHT packet"
                );
                return;
            }
        };

        match msg {
            KrpcMessage::Query {
                transaction_id,
                query,
            } => {
                self.remember_query_sender(&query, addr);
                self.handle_query(transaction_id, query, addr).await;
            }
            KrpcMessage::Response {
                transaction_id,
                response,
            } => {
                // TNG-019: only trust a response that matches a query we
                // actually sent, from the exact address we sent it to.
                // Without this, any UDP packet claiming a live/guessable
                // transaction id -- from any source -- was merged straight
                // into the routing table and, for get_peers, forwarded to
                // the torrent as if it were real peer data.
                let Some(outstanding) = self.outstanding.get(&transaction_id).copied() else {
                    debug!(
                        component = "dht",
                        operation = "handle_response",
                        peer = %addr,
                        result = "rejected",
                        reason = "unknown transaction id",
                        "ignoring unsolicited DHT response"
                    );
                    return;
                };
                if outstanding.addr != addr {
                    warn!(
                        component = "dht",
                        operation = "handle_response",
                        peer = %addr,
                        expected = %outstanding.addr,
                        result = "rejected",
                        reason = "source address mismatch",
                        "ignoring DHT response whose source does not match the address its transaction id was queried at"
                    );
                    return;
                }
                self.outstanding.remove(&transaction_id);
                self.remember_node(response.id, addr);
                for node in response.nodes {
                    self.table.insert(node);
                }
                if let DhtRequest::GetPeers(info_hash) = outstanding.request {
                    if let Some(token) = response.token {
                        self.announce_peer_to_node(info_hash, token, addr).await;
                    }
                    self.forward_peers(info_hash, response.values).await;
                    self.continue_lookup(info_hash).await;
                }
                debug!(
                    nodes = self.table.total_nodes(),
                    "DHT routing table updated"
                );
            }
            KrpcMessage::Error {
                transaction_id,
                error,
            } => {
                // Same source check as Response: an error for a transaction
                // id we don't recognize, or from an address that doesn't
                // match who we sent it to, is not something we should let
                // clear our own outstanding query.
                match self.outstanding.get(&transaction_id) {
                    Some(outstanding) if outstanding.addr == addr => {
                        self.outstanding.remove(&transaction_id);
                    }
                    Some(outstanding) => {
                        warn!(
                            component = "dht",
                            operation = "handle_error",
                            peer = %addr,
                            expected = %outstanding.addr,
                            result = "rejected",
                            reason = "source address mismatch",
                            "ignoring DHT error whose source does not match the address its transaction id was queried at"
                        );
                        return;
                    }
                    None => {}
                }
                debug!(
                    peer = %addr,
                    code = error.code,
                    message = %error.message,
                    "DHT error response"
                );
            }
        }
    }

    async fn handle_query(&mut self, transaction_id: Vec<u8>, query: DhtQuery, addr: SocketAddr) {
        let response = match query {
            DhtQuery::Ping { .. } => KrpcMessage::Response {
                transaction_id,
                response: DhtResponse::new(self.local_id),
            },
            DhtQuery::FindNode { target, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.closest_response(target),
            },
            DhtQuery::GetPeers { info_hash, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.get_peers_response(info_hash, addr),
            },
            DhtQuery::AnnouncePeer {
                implied_port,
                info_hash,
                port,
                token,
                ..
            } => self.handle_announce_peer(
                transaction_id,
                addr,
                implied_port,
                info_hash,
                port,
                token,
            ),
        };
        if let Err(e) = self.socket.send_to(&response.encode(), addr).await {
            warn!(
                component = "dht",
                operation = "send_response",
                peer = %addr,
                result = "error",
                error = %e,
                "DHT response send failed"
            );
        }
    }

    fn closest_response(&self, target: NodeId) -> DhtResponse {
        let mut response = DhtResponse::new(self.local_id);
        response.nodes = self
            .table
            .closest(&target, K)
            .into_iter()
            .cloned()
            .collect();
        response
    }

    fn get_peers_response(&self, info_hash: [u8; 20], addr: SocketAddr) -> DhtResponse {
        let mut response = self.closest_response(NodeId::from_bytes(info_hash));
        response.token = Some(self.token_for_addr(addr));
        if let Some(peers) = self.announced_peers.get(&info_hash) {
            response.values = peers.clone();
            response.nodes.clear();
        }
        response
    }

    fn handle_announce_peer(
        &mut self,
        transaction_id: Vec<u8>,
        addr: SocketAddr,
        implied_port: bool,
        info_hash: [u8; 20],
        port: u16,
        token: Vec<u8>,
    ) -> KrpcMessage {
        if token != self.token_for_addr(addr) {
            return KrpcMessage::Error {
                transaction_id,
                error: DhtError {
                    code: 203,
                    message: "bad token".to_owned(),
                },
            };
        }
        let peer = if implied_port {
            addr
        } else {
            socket_addr_with_port(addr, port)
        };
        remember_announced_peer(
            self.announced_peers.entry(info_hash).or_default(),
            peer,
            DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP,
        );
        KrpcMessage::Response {
            transaction_id,
            response: DhtResponse::new(self.local_id),
        }
    }

    fn token_for_addr(&self, addr: SocketAddr) -> Vec<u8> {
        let mut token = Vec::with_capacity(8);
        match addr.ip() {
            IpAddr::V4(ip) => token.extend_from_slice(&ip.octets()),
            IpAddr::V6(ip) => token.extend_from_slice(&ip.octets()[..4]),
        }
        token.extend_from_slice(&self.local_id.as_bytes()[..4]);
        token
    }

    async fn search_torrents(&mut self) {
        for info_hash in self.torrents.keys().copied().collect::<Vec<_>>() {
            self.search_torrent(info_hash, false).await;
        }
    }

    async fn search_torrent(&mut self, info_hash: [u8; 20], force_restart: bool) {
        self.maybe_restart_lookup(info_hash, force_restart);
        let target = NodeId::from_bytes(info_hash);
        let nodes: Vec<_> = self
            .table
            .closest(&target, K)
            .into_iter()
            .map(|node| SocketAddr::V4(node.addr))
            .collect();
        if nodes.is_empty() {
            self.bootstrap().await;
            return;
        }
        for addr in nodes {
            self.send_get_peers(info_hash, addr).await;
        }
    }

    fn maybe_restart_lookup(&mut self, info_hash: [u8; 20], force_restart: bool) {
        const DHT_LOOKUP_RESTART_AFTER: Duration = Duration::from_secs(120);
        let now = Instant::now();
        let should_restart = force_restart
            || self
                .last_full_lookup
                .get(&info_hash)
                .map(|last| now.duration_since(*last) >= DHT_LOOKUP_RESTART_AFTER)
                .unwrap_or(true);
        if should_restart {
            self.queried_nodes.remove(&info_hash);
            self.last_full_lookup.insert(info_hash, now);
        }
    }

    async fn continue_lookup(&mut self, info_hash: [u8; 20]) {
        if !self.torrents.contains_key(&info_hash) {
            return;
        }
        let target = NodeId::from_bytes(info_hash);
        let addrs: Vec<_> = self
            .table
            .closest(&target, K)
            .into_iter()
            .filter_map(|node| {
                let addr = node.addr;
                let already_queried = self
                    .queried_nodes
                    .get(&info_hash)
                    .is_some_and(|nodes| nodes.contains(&addr));
                (!already_queried).then_some(SocketAddr::V4(addr))
            })
            .collect();

        for addr in addrs {
            self.send_get_peers(info_hash, addr).await;
        }
    }

    async fn send_get_peers(&mut self, info_hash: [u8; 20], addr: SocketAddr) {
        let SocketAddr::V4(v4) = addr else {
            return;
        };
        let queried = self.queried_nodes.entry(info_hash).or_default();
        if !queried.insert(v4) {
            return;
        }
        let tx = self.transaction_id();
        let msg = KrpcMessage::Query {
            transaction_id: tx.clone(),
            query: DhtQuery::GetPeers {
                id: self.local_id,
                info_hash,
            },
        };
        self.outstanding.insert(
            tx,
            OutstandingQuery {
                addr,
                request: DhtRequest::GetPeers(info_hash),
                sent_at: Instant::now(),
            },
        );
        if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
            warn!(
                component = "dht",
                operation = "send_get_peers",
                peer = %addr,
                result = "error",
                error = %e,
                "DHT get_peers send failed"
            );
        }
    }

    async fn announce_peer_to_node(
        &mut self,
        info_hash: [u8; 20],
        token: Vec<u8>,
        addr: SocketAddr,
    ) {
        let (tx, msg) = self.announce_peer_query(info_hash, token);
        self.outstanding.insert(
            tx,
            OutstandingQuery {
                addr,
                request: DhtRequest::AnnouncePeer,
                sent_at: Instant::now(),
            },
        );
        if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
            warn!(
                component = "dht",
                operation = "send_announce_peer",
                peer = %addr,
                result = "error",
                error = %e,
                "DHT announce_peer send failed"
            );
        }
    }

    fn announce_peer_query(
        &mut self,
        info_hash: [u8; 20],
        token: Vec<u8>,
    ) -> (Vec<u8>, KrpcMessage) {
        let transaction_id = self.transaction_id();
        let msg = KrpcMessage::Query {
            transaction_id: transaction_id.clone(),
            query: DhtQuery::AnnouncePeer {
                id: self.local_id,
                implied_port: false,
                info_hash,
                port: self.listen_port,
                token,
            },
        };
        (transaction_id, msg)
    }

    async fn forward_peers(&mut self, info_hash: [u8; 20], peers: Vec<SocketAddr>) {
        if peers.is_empty() {
            return;
        }
        let Some(tx) = self.torrents.get(&info_hash) else {
            return;
        };
        if tx.send(TorrentCmd::NewPeers(peers)).await.is_err() {
            self.torrents.remove(&info_hash);
        }
    }

    fn remember_query_sender(&mut self, query: &DhtQuery, addr: SocketAddr) {
        let id = match query {
            DhtQuery::Ping { id }
            | DhtQuery::FindNode { id, .. }
            | DhtQuery::GetPeers { id, .. }
            | DhtQuery::AnnouncePeer { id, .. } => *id,
        };
        self.remember_node(id, addr);
    }

    fn remember_node(&mut self, id: NodeId, addr: SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            self.table.insert(KNode { id, addr });
        }
    }

    fn runtime_stats(&self) -> DhtRuntimeStats {
        DhtRuntimeStats {
            routing_nodes: self.table.total_nodes() as u64,
            announced_peer_sets: self.announced_peers.len() as u64,
            announced_peers: self
                .announced_peers
                .values()
                .map(|peers| peers.len() as u64)
                .sum(),
            tracked_torrents: self.torrents.len() as u64,
            outstanding_requests: self.outstanding.len() as u64,
            queried_nodes: self
                .queried_nodes
                .values()
                .map(|nodes| nodes.len() as u64)
                .sum(),
        }
    }

    fn transaction_id(&mut self) -> Vec<u8> {
        let tx = self.next_tx.to_be_bytes().to_vec();
        self.next_tx = self.next_tx.wrapping_add(1).max(1);
        tx
    }

    /// TNG-019: drops outstanding queries that never got a response within
    /// `OUTSTANDING_QUERY_TTL`. Without this, a node that silently drops our
    /// packets (or is offline, or is deliberately ignoring us) leaves an
    /// entry in `outstanding` forever -- an unbounded, attacker-triggerable
    /// growth path (send queries to many never-responding addresses) as
    /// well as ordinary leak from normal network loss.
    fn prune_stale_outstanding(&mut self) {
        let before = self.outstanding.len();
        self.outstanding
            .retain(|_, query| query.sent_at.elapsed() < OUTSTANDING_QUERY_TTL);
        let pruned = before - self.outstanding.len();
        if pruned > 0 {
            debug!(
                component = "dht",
                operation = "prune_stale_outstanding",
                pruned,
                remaining = self.outstanding.len(),
                "pruned stale outstanding DHT queries"
            );
        }
    }
}

fn remember_announced_peer(peers: &mut Vec<SocketAddr>, peer: SocketAddr, cap: usize) -> bool {
    if peers.contains(&peer) {
        return true;
    }
    if peers.len() >= cap {
        return false;
    }
    peers.push(peer);
    true
}

fn socket_addr_with_port(addr: SocketAddr, port: u16) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(*v4.ip(), port)),
        SocketAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(*v6.ip(), port, 0, v6.scope_id())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transaction_ids_are_nonzero_and_advance() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: u16::MAX,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };

        assert_eq!(task.transaction_id(), u16::MAX.to_be_bytes());
        assert_eq!(task.transaction_id(), 1u16.to_be_bytes());
    }

    #[tokio::test]
    async fn closest_response_includes_known_nodes_and_token() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        task.table.insert(KNode {
            id: NodeId::from_bytes([2; 20]),
            addr: std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6881),
        });

        let response = task.closest_response(NodeId::from_bytes([3; 20]));
        assert_eq!(response.id, local_id);
        assert_eq!(response.nodes.len(), 1);
        assert!(response.token.is_none());
    }

    #[tokio::test]
    async fn announce_peer_requires_matching_token() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let addr: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        let response =
            task.handle_announce_peer(b"aa".to_vec(), addr, true, [9; 20], 6881, b"bad".to_vec());
        assert!(matches!(response, KrpcMessage::Error { .. }));
        assert!(task.announced_peers.is_empty());
    }

    #[tokio::test]
    async fn announce_peer_stores_peer_and_get_peers_returns_it() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let info_hash = [9; 20];
        let addr: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        let token = task.token_for_addr(addr);
        let response =
            task.handle_announce_peer(b"aa".to_vec(), addr, false, info_hash, 6881, token);
        assert!(matches!(response, KrpcMessage::Response { .. }));

        let peers = task.get_peers_response(info_hash, addr);
        assert_eq!(peers.values, vec!["127.0.0.1:6881".parse().unwrap()]);
        assert!(peers.nodes.is_empty());
        assert!(peers.token.is_some());
    }

    #[tokio::test]
    async fn announce_peer_stores_ipv6_peer_and_get_peers_returns_it() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let info_hash = [9; 20];
        let addr: SocketAddr = "[2001:db8::1]:60000".parse().unwrap();
        let token = task.token_for_addr(addr);
        let response =
            task.handle_announce_peer(b"aa".to_vec(), addr, false, info_hash, 6881, token);
        assert!(matches!(response, KrpcMessage::Response { .. }));

        let peers = task.get_peers_response(info_hash, addr);
        assert_eq!(peers.values, vec!["[2001:db8::1]:6881".parse().unwrap()]);
        assert!(peers.nodes.is_empty());
        assert!(peers.token.is_some());
    }

    #[test]
    fn announced_peer_cache_is_bounded_and_keeps_duplicates() {
        let first: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let second: SocketAddr = "127.0.0.2:6881".parse().unwrap();
        let third: SocketAddr = "127.0.0.3:6881".parse().unwrap();
        let mut peers = Vec::new();

        assert!(remember_announced_peer(&mut peers, first, 2));
        assert!(remember_announced_peer(&mut peers, second, 2));
        assert!(remember_announced_peer(&mut peers, first, 2));
        assert!(!remember_announced_peer(&mut peers, third, 2));

        assert_eq!(peers, vec![first, second]);
    }

    #[tokio::test]
    async fn runtime_stats_count_dht_owned_caches() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let info_hash = [9; 20];
        let queried = "127.0.0.1:6001".parse().unwrap();
        let announced: SocketAddr = "127.0.0.1:6881".parse().unwrap();
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::from([(
                b"aa".to_vec(),
                OutstandingQuery {
                    addr: SocketAddr::from(queried),
                    request: DhtRequest::Bootstrap,
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([queried]))]),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::from([(info_hash, vec![announced])]),
            last_full_lookup: HashMap::new(),
        };
        task.table.insert(KNode {
            id: NodeId::from_bytes([2; 20]),
            addr: "127.0.0.2:6881".parse().unwrap(),
        });

        let stats = task.runtime_stats();

        assert_eq!(stats.routing_nodes, 1);
        assert_eq!(stats.announced_peer_sets, 1);
        assert_eq!(stats.announced_peers, 1);
        assert_eq!(stats.tracked_torrents, 1);
        assert_eq!(stats.outstanding_requests, 1);
        assert_eq!(stats.queried_nodes, 1);
    }

    #[tokio::test]
    async fn announce_peer_query_uses_configured_listen_port() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 51413,
            bootstrap_nodes: Vec::new(),
            next_tx: 7,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let token = b"token".to_vec();
        let (tx, msg) = task.announce_peer_query([9; 20], token.clone());

        assert_eq!(tx, 7u16.to_be_bytes());
        match msg {
            KrpcMessage::Query {
                transaction_id,
                query:
                    DhtQuery::AnnouncePeer {
                        id,
                        implied_port,
                        info_hash,
                        port,
                        token: query_token,
                    },
            } => {
                assert_eq!(transaction_id, 7u16.to_be_bytes());
                assert_eq!(id, local_id);
                assert!(!implied_port);
                assert_eq!(info_hash, [9; 20]);
                assert_eq!(port, 51413);
                assert_eq!(query_token, token);
            }
            other => panic!("unexpected KRPC message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn lookup_continues_to_unqueried_closer_nodes() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let info_hash = [9; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let first = std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6001);
        let second = std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6002);
        task.table.insert(KNode {
            id: NodeId::from_bytes([2; 20]),
            addr: first,
        });

        task.search_torrent(info_hash, true).await;
        assert!(task.queried_nodes[&info_hash].contains(&first));
        assert_eq!(task.outstanding.len(), 1);

        task.table.insert(KNode {
            id: NodeId::from_bytes([3; 20]),
            addr: second,
        });
        task.continue_lookup(info_hash).await;

        assert!(task.queried_nodes[&info_hash].contains(&second));
        assert_eq!(task.outstanding.len(), 2);
    }

    #[tokio::test]
    async fn lookup_restart_clears_previously_queried_nodes() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let info_hash = [9; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let first = std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6001);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([first]))]),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        task.table.insert(KNode {
            id: NodeId::from_bytes([2; 20]),
            addr: first,
        });

        task.search_torrent(info_hash, true).await;

        assert!(task.queried_nodes[&info_hash].contains(&first));
        assert_eq!(task.outstanding.len(), 1);
    }

    #[tokio::test]
    async fn get_peers_response_forwards_discovered_peers_to_torrent() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let remote_id = NodeId::from_bytes([2; 20]);
        let info_hash = [9; 20];
        let discovered_peer: SocketAddr = "127.0.0.1:51413".parse().unwrap();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::from([(
                b"gp".to_vec(),
                OutstandingQuery {
                    addr: "127.0.0.1:6001".parse().unwrap(),
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::new(),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"gp".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                values: vec![discovered_peer],
                token: None,
            },
        };

        task.handle_packet(&response.encode(), "127.0.0.1:6001".parse().unwrap())
            .await;

        match cmd_rx.recv().await {
            Some(TorrentCmd::NewPeers(peers)) => {
                assert_eq!(peers, vec![discovered_peer]);
            }
            other => panic!("unexpected torrent command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn response_from_wrong_source_address_is_ignored() {
        // TNG-019: a response claiming a transaction id we really did send,
        // but arriving from a different address than the one we sent it
        // to, must not be treated as real -- otherwise any off-path
        // attacker who guesses/observes a transaction id could inject
        // forged peers/nodes from anywhere.
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let remote_id = NodeId::from_bytes([2; 20]);
        let info_hash = [9; 20];
        let queried_addr: SocketAddr = "127.0.0.1:6001".parse().unwrap();
        let spoofed_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let discovered_peer: SocketAddr = "127.0.0.1:51413".parse().unwrap();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::from([(
                b"gp".to_vec(),
                OutstandingQuery {
                    addr: queried_addr,
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::new(),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"gp".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                values: vec![discovered_peer],
                token: None,
            },
        };

        task.handle_packet(&response.encode(), spoofed_addr).await;

        assert!(
            cmd_rx.try_recv().is_err(),
            "a spoofed response must not be forwarded as discovered peers"
        );
        assert_eq!(
            task.table.total_nodes(),
            0,
            "a spoofed response must not be merged into the routing table"
        );
        assert_eq!(
            task.outstanding.len(),
            1,
            "the real outstanding query must remain pending, not be consumed by a spoofed reply"
        );
    }

    #[tokio::test]
    async fn response_with_unknown_transaction_id_is_ignored() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let remote_id = NodeId::from_bytes([2; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"zz".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                values: Vec::new(),
                token: None,
            },
        };

        task.handle_packet(&response.encode(), "127.0.0.1:9999".parse().unwrap())
            .await;

        assert_eq!(
            task.table.total_nodes(),
            0,
            "an unsolicited response must not be merged into the routing table"
        );
    }

    #[tokio::test]
    async fn prune_stale_outstanding_removes_expired_entries_only() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let addr: SocketAddr = "127.0.0.1:6001".parse().unwrap();
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::from([
                (
                    b"old".to_vec(),
                    OutstandingQuery {
                        addr,
                        request: DhtRequest::Bootstrap,
                        sent_at: Instant::now() - OUTSTANDING_QUERY_TTL - Duration::from_secs(1),
                    },
                ),
                (
                    b"new".to_vec(),
                    OutstandingQuery {
                        addr,
                        request: DhtRequest::Bootstrap,
                        sent_at: Instant::now(),
                    },
                ),
            ]),
            queried_nodes: HashMap::new(),
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
        };

        task.prune_stale_outstanding();

        assert_eq!(task.outstanding.len(), 1);
        assert!(task.outstanding.contains_key(b"new".as_slice()));
    }
}
