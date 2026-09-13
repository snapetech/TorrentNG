//! Minimal BEP 5 DHT service loop.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use rt_dht::{DhtError, DhtQuery, DhtResponse, KNode, KrpcMessage, NodeId, RoutingTable, K};
use sha1::{Digest, Sha1};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::torrent_task::TorrentCmd;

const DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP: usize = 512;
const DHT_ANNOUNCED_PEER_SET_CAP: usize = 4_096;
const DHT_ANNOUNCED_PEERS_GLOBAL_CAP: usize = 16_384;
const DHT_TRACKED_TORRENTS_CAP: usize = 16_384;
const DHT_COMMAND_GENERATION_CAP: usize = DHT_TRACKED_TORRENTS_CAP.saturating_mul(2);
const DHT_QUERIED_NODES_PER_INFO_HASH_CAP: usize = 256;
// Keep the aggregate queried-node history bounded across all tracked
// torrents. The old per-info-hash limit alone allowed 16,384 torrents to
// retain more than four million socket addresses before any admission path
// noticed the growth.
const DHT_QUERIED_NODES_GLOBAL_CAP: usize = 262_144;
const DHT_OUTSTANDING_QUERY_CAP: usize = 8_192;
const DHT_PENDING_FORWARD_TORRENT_CAP: usize = 1_024;
const DHT_PENDING_FORWARD_PEERS_PER_TORRENT_CAP: usize = 512;
// Bound the aggregate peer addresses retained while torrent mailboxes are
// full. The per-torrent cap alone allowed 1,024 stalled torrents to retain
// more than half a million SocketAddr values.
const DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP: usize = 65_536;
const DHT_INGRESS_GLOBAL_PACKETS_PER_SECOND: u32 = 2_048;
const DHT_INGRESS_PACKETS_PER_IP_PER_SECOND: u32 = 64;
const DHT_INGRESS_IP_STATE_CAP: usize = 4_096;
const DHT_INGRESS_WINDOW: Duration = Duration::from_secs(1);
const DHT_MAX_DATAGRAM_LEN: usize = 2_048;
const DHT_MAX_RESPONSE_DATAGRAM_LEN: usize = 1_024;
const DHT_TOKEN_ROTATION: Duration = Duration::from_secs(5 * 60);
const DHT_TOKEN_ACCEPTANCE: Duration = Duration::from_secs(10 * 60);

// The node ID is sent in ordinary DHT responses and therefore cannot be used
// as an announce-token secret. Keep rotating unpredictable secrets for the
// daemon process; tokens remain address-bound but cannot be forged by deriving
// them from the public node ID. The previous secret is retained for the
// protocol's acceptance window so peers can announce after a rotation.
struct DhtTokenSecrets {
    // Newest first. Three entries cover the current five-minute epoch plus
    // the two preceding epochs, which keeps tokens valid for up to ten
    // minutes without allowing the cache to grow.
    values: Vec<([u8; 20], Instant)>,
}

impl DhtTokenSecrets {
    fn current(&mut self, now: Instant) -> Vec<[u8; 20]> {
        if self
            .values
            .first()
            .and_then(|(_, started_at)| now.checked_duration_since(*started_at))
            .is_some_and(|elapsed| elapsed >= DHT_TOKEN_ROTATION)
        {
            self.values
                .insert(0, (*rt_dht::NodeId::random().as_bytes(), now));
            self.values.truncate(3);
        }

        let current = self
            .values
            .iter()
            .filter(|(_, started_at)| {
                now.checked_duration_since(*started_at)
                    .is_some_and(|elapsed| elapsed <= DHT_TOKEN_ACCEPTANCE)
            })
            .map(|(secret, _)| *secret)
            .collect::<Vec<_>>();

        if !current.is_empty() {
            return current;
        }

        // A backwards monotonic-clock observation, or a cache restored past
        // its acceptance window, must not turn token generation into a
        // process panic. Invalidate the old epochs and establish a fresh
        // current secret; callers that generated a token from it will see
        // the same secret on their subsequent validation call.
        let secret = *rt_dht::NodeId::random().as_bytes();
        self.values = vec![(secret, now)];
        vec![secret]
    }
}

static DHT_TOKEN_SECRETS: OnceLock<Mutex<DhtTokenSecrets>> = OnceLock::new();

fn dht_token_secrets(now: Instant) -> Vec<[u8; 20]> {
    let mutex = DHT_TOKEN_SECRETS.get_or_init(|| {
        Mutex::new(DhtTokenSecrets {
            values: vec![(*rt_dht::NodeId::random().as_bytes(), now)],
        })
    });
    let mut secrets = mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    secrets.current(now)
}

fn dht_token_for_secret(addr: SocketAddr, secret: &[u8; 20]) -> Vec<u8> {
    let mut hasher = Sha1::new();
    match addr.ip() {
        IpAddr::V4(ip) => hasher.update(ip.octets()),
        IpAddr::V6(ip) => hasher.update(ip.octets()),
    }
    hasher.update(secret);
    hasher.finalize().to_vec()
}

#[derive(Debug, Clone, Copy)]
struct DhtRateWindow {
    started_at: Instant,
    packets: u32,
}

/// Bounded admission control for the public UDP socket. KRPC parsing and
/// response generation are both CPU work, so rejecting excess datagrams
/// before bencode parsing is a correctness boundary, not merely a tuning
/// knob. Per-IP state is capped so spoofed/rotating sources cannot turn the
/// limiter itself into an unbounded allocation.
#[derive(Debug)]
struct DhtIngressBudget {
    global: DhtRateWindow,
    per_ip: HashMap<IpAddr, DhtRateWindow>,
}

impl DhtIngressBudget {
    fn new(now: Instant) -> Self {
        Self {
            global: DhtRateWindow {
                started_at: now,
                packets: 0,
            },
            per_ip: HashMap::new(),
        }
    }

    fn allow(&mut self, addr: SocketAddr, now: Instant) -> bool {
        if now.duration_since(self.global.started_at) >= DHT_INGRESS_WINDOW {
            self.global = DhtRateWindow {
                started_at: now,
                packets: 0,
            };
            self.per_ip.clear();
        }
        if self.global.packets >= DHT_INGRESS_GLOBAL_PACKETS_PER_SECOND {
            return false;
        }

        let ip = addr.ip();
        if !self.per_ip.contains_key(&ip) {
            if self.per_ip.len() >= DHT_INGRESS_IP_STATE_CAP {
                return false;
            }
            self.per_ip.insert(
                ip,
                DhtRateWindow {
                    started_at: now,
                    packets: 0,
                },
            );
        }
        let window = self.per_ip.get_mut(&ip).expect("inserted DHT IP budget");
        if now.duration_since(window.started_at) >= DHT_INGRESS_WINDOW {
            *window = DhtRateWindow {
                started_at: now,
                packets: 0,
            };
        }
        if window.packets >= DHT_INGRESS_PACKETS_PER_IP_PER_SECOND {
            return false;
        }
        window.packets += 1;
        self.global.packets += 1;
        true
    }
}

#[derive(Clone)]
pub struct DhtTorrent {
    pub info_hash: [u8; 20],
    pub cmd_tx: mpsc::Sender<TorrentCmd>,
    pub generation: u64,
}

pub enum DhtCommand {
    AddTorrent(DhtTorrent),
    RemoveTorrent {
        info_hash: [u8; 20],
        generation: u64,
    },
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
        generations: HashMap::new(),
        announced_peers: HashMap::new(),
        last_full_lookup: HashMap::new(),
        pending_peer_forwards: HashMap::new(),
    };
    task.bootstrap().await;

    let mut ingress_budget = DhtIngressBudget::new(Instant::now());
    let mut bootstrap_tick = interval(Duration::from_secs(300));
    let mut search_tick = interval(Duration::from_secs(30));
    let mut outstanding_sweep_tick = interval(Duration::from_secs(10));
    let mut pending_forward_tick = interval(Duration::from_millis(100));
    // Allocate one sentinel byte so recv_from can distinguish a datagram that
    // exceeds the parser's limit from a valid datagram that exactly fills the
    // buffer. Without the sentinel, an oversized UDP packet is truncated and
    // its prefix can be parsed as a different KRPC message.
    let mut buf = vec![0u8; DHT_MAX_DATAGRAM_LEN.saturating_add(1)];
    let mut shutdown_reply = None;
    loop {
        tokio::select! {
            command = cmd_rx.recv() => {
                let Some(cmd) = command else {
                    warn!(
                        component = "dht",
                        operation = "run",
                        result = "command_channel_closed",
                        "DHT command channel closed; shutting down"
                    );
                    break;
                };
                if let DhtCommand::Shutdown { reply } = cmd {
                    info!(
                        component = "dht",
                        operation = "shutdown",
                        result = "ok",
                        "DHT task shutting down"
                    );
                    shutdown_reply = Some(reply);
                    break;
                }
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
                for info_hash in task.prune_stale_outstanding() {
                    task.continue_lookup(info_hash).await;
                }
            }
            _ = pending_forward_tick.tick() => {
                task.flush_pending_peer_forwards();
            }
            recv = task.socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, addr)) => {
                        if !ingress_budget.allow(addr, Instant::now()) {
                            debug!(
                                component = "dht",
                                operation = "ingress_rate_limit",
                                peer = %addr,
                                result = "rejected",
                                "DHT packet rate limit exceeded"
                            );
                        } else if n > DHT_MAX_DATAGRAM_LEN {
                            debug!(
                                component = "dht",
                                operation = "ingress_size_limit",
                                peer = %addr,
                                bytes = n,
                                max_bytes = DHT_MAX_DATAGRAM_LEN,
                                result = "rejected",
                                "DHT datagram exceeds configured parser limit"
                            );
                        } else {
                            task.handle_packet(&buf[..n], addr).await;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e).context("receiving DHT UDP datagram"),
                }
            }
        }
    }
    // Drop the socket-owning task before acknowledging shutdown. Callers can
    // then safely bind the configured DHT port for a replacement engine.
    drop(task);
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
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
    /// Last accepted engine command generation, including removed torrents.
    /// Retaining bounded tombstones prevents a delayed Add from resurrecting
    /// a torrent after a later Remove was delivered first.
    generations: HashMap<[u8; 20], u64>,
    announced_peers: HashMap<[u8; 20], Vec<SocketAddr>>,
    last_full_lookup: HashMap<[u8; 20], Instant>,
    pending_peer_forwards: HashMap<[u8; 20], PendingPeerForward>,
}

struct PendingPeerForward {
    cmd_tx: mpsc::Sender<TorrentCmd>,
    peers: Vec<SocketAddr>,
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
    fn accept_generation(&mut self, info_hash: [u8; 20], generation: u64) -> bool {
        if self
            .generations
            .get(&info_hash)
            .is_some_and(|current| *current >= generation)
        {
            return false;
        }
        if !self.generations.contains_key(&info_hash)
            && self.generations.len() >= DHT_COMMAND_GENERATION_CAP
        {
            let evictable = self
                .generations
                .keys()
                .find(|candidate| !self.torrents.contains_key(*candidate))
                .copied();
            let Some(evictable) = evictable else {
                return false;
            };
            self.generations.remove(&evictable);
        }
        self.generations.insert(info_hash, generation);
        true
    }

    /// Discard lookup state belonging to a previous torrent-task incarnation.
    /// A late response for an old `get_peers` request must not be forwarded to
    /// a task that was added later for the same info-hash.
    fn clear_torrent_lookup_state(&mut self, info_hash: [u8; 20]) {
        self.outstanding.retain(|_, query| {
            !matches!(query.request, DhtRequest::GetPeers(candidate) if candidate == info_hash)
        });
        self.queried_nodes.remove(&info_hash);
        self.last_full_lookup.remove(&info_hash);
    }

    async fn handle_command(&mut self, cmd: DhtCommand) -> bool {
        match cmd {
            DhtCommand::AddTorrent(torrent) => {
                if !self.torrents.contains_key(&torrent.info_hash)
                    && self.torrents.len() >= DHT_TRACKED_TORRENTS_CAP
                {
                    warn!(
                        component = "dht",
                        operation = "track_torrent",
                        result = "rejected",
                        reason = "tracked torrent cap exceeded",
                        cap = DHT_TRACKED_TORRENTS_CAP,
                        "DHT tracking admission cap exceeded"
                    );
                    return true;
                }
                // Do not consume a generation for an add rejected by the
                // tracking-cap admission above. The same registration may be
                // retried after another torrent is removed; recording it as
                // accepted before admission would make that retry look stale
                // and lose DHT tracking permanently for this generation.
                if !self.accept_generation(torrent.info_hash, torrent.generation) {
                    return true;
                }
                self.clear_torrent_lookup_state(torrent.info_hash);
                self.pending_peer_forwards.remove(&torrent.info_hash);
                self.torrents.insert(torrent.info_hash, torrent.cmd_tx);
                self.search_torrent(torrent.info_hash, true).await;
            }
            DhtCommand::RemoveTorrent {
                info_hash,
                generation,
            } => {
                if !self.accept_generation(info_hash, generation) {
                    return true;
                }
                self.torrents.remove(&info_hash);
                self.clear_torrent_lookup_state(info_hash);
                self.announced_peers.remove(&info_hash);
                self.pending_peer_forwards.remove(&info_hash);
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
            let addrs =
                match tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(&node))
                    .await
                {
                    Ok(Ok(addrs)) => addrs,
                    Ok(Err(e)) => {
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
                    Err(_) => {
                        warn!(
                            component = "dht",
                            operation = "bootstrap_resolve",
                            node = %node,
                            result = "timeout",
                            "DHT bootstrap resolve timed out"
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
                if !self.insert_outstanding(
                    tx.clone(),
                    OutstandingQuery {
                        addr,
                        request: DhtRequest::Bootstrap,
                        sent_at: Instant::now(),
                    },
                ) {
                    break;
                }
                if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
                    self.outstanding.remove(&tx);
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
                let request = match self.outstanding.get(&transaction_id).copied() {
                    Some(outstanding) if outstanding.addr == addr => {
                        self.outstanding.remove(&transaction_id);
                        Some(outstanding.request)
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
                    None => None,
                };
                debug!(
                    peer = %addr,
                    code = error.code,
                    message = %error.message,
                    "DHT error response"
                );
                // An error still completes a lookup round. Continue with
                // other known nodes instead of waiting for the periodic
                // lookup restart, otherwise one rejecting/unsupported node
                // can stall peer discovery for the torrent.
                if let Some(DhtRequest::GetPeers(info_hash)) = request {
                    self.continue_lookup(info_hash).await;
                }
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
        let Some(encoded) = encode_dht_response_bounded(response) else {
            warn!(
                component = "dht",
                operation = "send_response",
                peer = %addr,
                result = "rejected",
                max_bytes = DHT_MAX_RESPONSE_DATAGRAM_LEN,
                "DHT response does not fit the configured UDP payload limit"
            );
            return;
        };
        if let Err(e) = self.socket.send_to(&encoded, addr).await {
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
        if !dht_token_secrets(Instant::now())
            .iter()
            .any(|secret| dht_token_for_secret(addr, secret) == token)
        {
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
        remember_announced_peer_in_map(&mut self.announced_peers, info_hash, peer);
        KrpcMessage::Response {
            transaction_id,
            response: DhtResponse::new(self.local_id),
        }
    }

    fn token_for_addr(&self, addr: SocketAddr) -> Vec<u8> {
        // Bind the token to the complete source address. Using only a prefix
        // for IPv6 made all hosts sharing that prefix interchangeable for
        // announce_peer, which defeats the address-binding check. The secret
        // input must not come from `local_id`: that ID is public in every
        // response, so doing so would let any peer mint valid tokens.
        let secret = dht_token_secrets(Instant::now())
            .into_iter()
            .next()
            .expect("DHT token secret cache always has a current secret");
        dht_token_for_secret(addr, &secret)
    }

    async fn search_torrents(&mut self) {
        let mut query_budget = DHT_QUERIED_NODES_GLOBAL_CAP;
        for info_hash in self.torrents.keys().copied().collect::<Vec<_>>() {
            if query_budget == 0 {
                break;
            }
            query_budget = self
                .search_torrent_with_budget(info_hash, false, query_budget)
                .await;
        }
    }

    async fn search_torrent(&mut self, info_hash: [u8; 20], force_restart: bool) {
        self.search_torrent_with_budget(info_hash, force_restart, DHT_QUERIED_NODES_GLOBAL_CAP)
            .await;
    }

    async fn search_torrent_with_budget(
        &mut self,
        info_hash: [u8; 20],
        force_restart: bool,
        mut query_budget: usize,
    ) -> usize {
        self.maybe_restart_lookup(info_hash, force_restart);
        query_budget = self
            .remaining_queried_node_budget()
            .min(query_budget)
            .min(DHT_QUERIED_NODES_GLOBAL_CAP);
        let target = NodeId::from_bytes(info_hash);
        let nodes: Vec<_> = self
            .table
            .closest(&target, K)
            .into_iter()
            .map(|node| SocketAddr::V4(node.addr))
            .collect();
        if nodes.is_empty() {
            self.bootstrap().await;
            return query_budget;
        }
        for addr in nodes {
            if query_budget == 0 {
                break;
            }
            if self.send_get_peers(info_hash, addr, query_budget).await {
                query_budget -= 1;
            }
        }
        query_budget
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
        let mut query_budget = self
            .remaining_queried_node_budget()
            .min(DHT_QUERIED_NODES_GLOBAL_CAP);
        let target = NodeId::from_bytes(info_hash);
        let addrs: Vec<_> = self
            .table
            // A response or timeout can make one of the initial K candidates
            // unusable. Look beyond that initial window so a silent node does
            // not block the rest of the routing table; retain K-way fanout so
            // one response cannot flood the UDP socket with every known node.
            .closest(&target, DHT_QUERIED_NODES_PER_INFO_HASH_CAP)
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

        for addr in addrs.into_iter().take(K) {
            if query_budget == 0 {
                break;
            }
            if self.send_get_peers(info_hash, addr, query_budget).await {
                query_budget -= 1;
            }
        }
    }

    fn remaining_queried_node_budget(&self) -> usize {
        let used = self
            .queried_nodes
            .values()
            .fold(0usize, |total, nodes| total.saturating_add(nodes.len()));
        DHT_QUERIED_NODES_GLOBAL_CAP.saturating_sub(used)
    }

    async fn send_get_peers(
        &mut self,
        info_hash: [u8; 20],
        addr: SocketAddr,
        query_budget: usize,
    ) -> bool {
        let SocketAddr::V4(v4) = addr else {
            return false;
        };
        if query_budget == 0 {
            return false;
        }
        if self.outstanding.len() >= DHT_OUTSTANDING_QUERY_CAP {
            return false;
        }
        if self
            .queried_nodes
            .get(&info_hash)
            .is_some_and(|nodes| nodes.len() >= DHT_QUERIED_NODES_PER_INFO_HASH_CAP)
        {
            return false;
        }
        if !self.queried_nodes.entry(info_hash).or_default().insert(v4) {
            return false;
        }
        let tx = self.transaction_id();
        let msg = KrpcMessage::Query {
            transaction_id: tx.clone(),
            query: DhtQuery::GetPeers {
                id: self.local_id,
                info_hash,
            },
        };
        if !self.insert_outstanding(
            tx.clone(),
            OutstandingQuery {
                addr,
                request: DhtRequest::GetPeers(info_hash),
                sent_at: Instant::now(),
            },
        ) {
            let empty = if let Some(queried) = self.queried_nodes.get_mut(&info_hash) {
                queried.remove(&v4);
                queried.is_empty()
            } else {
                false
            };
            if empty {
                self.queried_nodes.remove(&info_hash);
            }
            return false;
        }
        if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
            self.outstanding.remove(&tx);
            let empty = if let Some(queried) = self.queried_nodes.get_mut(&info_hash) {
                queried.remove(&v4);
                queried.is_empty()
            } else {
                false
            };
            if empty {
                self.queried_nodes.remove(&info_hash);
            }
            warn!(
                component = "dht",
                operation = "send_get_peers",
                peer = %addr,
                result = "error",
                error = %e,
                "DHT get_peers send failed"
            );
            return false;
        }
        true
    }

    async fn announce_peer_to_node(
        &mut self,
        info_hash: [u8; 20],
        token: Vec<u8>,
        addr: SocketAddr,
    ) {
        let (tx, msg) = self.announce_peer_query(info_hash, token);
        if !self.insert_outstanding(
            tx.clone(),
            OutstandingQuery {
                addr,
                request: DhtRequest::AnnouncePeer,
                sent_at: Instant::now(),
            },
        ) {
            return;
        }
        if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
            self.outstanding.remove(&tx);
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
        let Some(tx) = self.torrents.get(&info_hash).cloned() else {
            return;
        };

        // Preserve ordering when an earlier batch is waiting for mailbox
        // capacity. Sending a later batch directly could make the old batch
        // arrive after it, and bypassing the pending queue would still leave
        // the original discovery result stranded.
        if self.pending_peer_forwards.contains_key(&info_hash) {
            self.queue_pending_peer_forward(info_hash, tx, peers);
            self.flush_pending_peer_forward(info_hash);
            return;
        }

        match tx.try_send(TorrentCmd::NewPeers(peers)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.torrents.remove(&info_hash);
            }
            Err(mpsc::error::TrySendError::Full(TorrentCmd::NewPeers(peers))) => {
                self.queue_pending_peer_forward(info_hash, tx, peers);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                unreachable!("forward_peers only sends TorrentCmd::NewPeers")
            }
        }
    }

    fn queue_pending_peer_forward(
        &mut self,
        info_hash: [u8; 20],
        cmd_tx: mpsc::Sender<TorrentCmd>,
        peers: Vec<SocketAddr>,
    ) {
        let mut pending_peer_count = self
            .pending_peer_forwards
            .values()
            .fold(0usize, |total, pending| {
                total.saturating_add(pending.peers.len())
            });
        if !self.pending_peer_forwards.contains_key(&info_hash)
            && pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
        {
            debug!(
                component = "dht",
                operation = "forward_peers",
                result = "pending_peer_global_cap_exceeded",
                cap = DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP,
                "dropping DHT peers because the global pending-forward peer cap is full"
            );
            return;
        }
        if !self.pending_peer_forwards.contains_key(&info_hash)
            && self.pending_peer_forwards.len() >= DHT_PENDING_FORWARD_TORRENT_CAP
        {
            debug!(
                component = "dht",
                operation = "forward_peers",
                result = "pending_cap_exceeded",
                cap = DHT_PENDING_FORWARD_TORRENT_CAP,
                "dropping DHT peers because the pending-forward cap is full"
            );
            return;
        }

        let pending = self
            .pending_peer_forwards
            .entry(info_hash)
            .or_insert_with(|| PendingPeerForward {
                cmd_tx,
                peers: Vec::new(),
            });
        for peer in peers {
            if pending.peers.contains(&peer) {
                continue;
            }
            if pending.peers.len() >= DHT_PENDING_FORWARD_PEERS_PER_TORRENT_CAP {
                debug!(
                    component = "dht",
                    operation = "forward_peers",
                    result = "pending_peer_cap_exceeded",
                    cap = DHT_PENDING_FORWARD_PEERS_PER_TORRENT_CAP,
                    "dropping excess duplicate-free DHT peers for a busy torrent"
                );
                break;
            }
            if pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP {
                debug!(
                    component = "dht",
                    operation = "forward_peers",
                    result = "pending_peer_global_cap_exceeded",
                    cap = DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP,
                    "dropping excess DHT peers because the global pending-forward peer cap is full"
                );
                break;
            }
            pending.peers.push(peer);
            pending_peer_count = pending_peer_count.saturating_add(1);
        }
    }

    fn flush_pending_peer_forwards(&mut self) {
        let info_hashes = self
            .pending_peer_forwards
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for info_hash in info_hashes {
            self.flush_pending_peer_forward(info_hash);
        }
    }

    fn flush_pending_peer_forward(&mut self, info_hash: [u8; 20]) {
        let Some(pending) = self.pending_peer_forwards.remove(&info_hash) else {
            return;
        };
        match pending.cmd_tx.try_send(TorrentCmd::NewPeers(pending.peers)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(TorrentCmd::NewPeers(peers))) => {
                self.pending_peer_forwards.insert(
                    info_hash,
                    PendingPeerForward {
                        cmd_tx: pending.cmd_tx,
                        peers,
                    },
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.torrents.remove(&info_hash);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                unreachable!("flush_pending_peer_forward only sends TorrentCmd::NewPeers")
            }
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
        loop {
            let tx = self.next_tx.to_be_bytes().to_vec();
            self.next_tx = self.next_tx.wrapping_add(1).max(1);
            if !self.outstanding.contains_key(&tx) {
                return tx;
            }
        }
    }

    fn insert_outstanding(&mut self, tx: Vec<u8>, query: OutstandingQuery) -> bool {
        if self.outstanding.len() >= DHT_OUTSTANDING_QUERY_CAP
            && !self.outstanding.contains_key(&tx)
        {
            debug!(
                component = "dht",
                operation = "track_outstanding_query",
                result = "rejected",
                cap = DHT_OUTSTANDING_QUERY_CAP,
                "DHT outstanding-query cap exceeded"
            );
            return false;
        }
        self.outstanding.insert(tx, query);
        true
    }

    /// TNG-019: drops outstanding queries that never got a response within
    /// `OUTSTANDING_QUERY_TTL`. Without this, a node that silently drops our
    /// packets (or is offline, or is deliberately ignoring us) leaves an
    /// entry in `outstanding` forever -- an unbounded, attacker-triggerable
    /// growth path (send queries to many never-responding addresses) as
    /// well as ordinary leak from normal network loss.
    fn prune_stale_outstanding(&mut self) -> Vec<[u8; 20]> {
        let before = self.outstanding.len();
        let mut lookup_info_hashes = HashSet::new();
        self.outstanding.retain(|_, query| {
            let fresh = query.sent_at.elapsed() < OUTSTANDING_QUERY_TTL;
            if !fresh {
                if let DhtRequest::GetPeers(info_hash) = query.request {
                    lookup_info_hashes.insert(info_hash);
                }
            }
            fresh
        });
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
        lookup_info_hashes.into_iter().collect()
    }
}

/// Encode a response without allowing the announced peer list to turn into a
/// fragmented UDP datagram. Values are truncated to the largest prefix that
/// fits after all bencode framing, the transaction id, and the token have
/// been accounted for. A response with no values is still retained so a
/// caller can receive the token and continue the lookup.
fn encode_dht_response_bounded(message: KrpcMessage) -> Option<Vec<u8>> {
    let KrpcMessage::Response {
        transaction_id,
        response,
    } = message
    else {
        let encoded = message.encode();
        return (encoded.len() <= DHT_MAX_RESPONSE_DATAGRAM_LEN).then_some(encoded);
    };

    let mut low = 0usize;
    let mut high = response.values.len();
    let mut best = None;
    while low <= high {
        let count = low + (high - low) / 2;
        let mut candidate = response.clone();
        candidate.values.truncate(count);
        let encoded = KrpcMessage::Response {
            transaction_id: transaction_id.clone(),
            response: candidate,
        }
        .encode();
        if encoded.len() <= DHT_MAX_RESPONSE_DATAGRAM_LEN {
            best = Some((count, encoded));
            low = count.saturating_add(1);
        } else if count == 0 {
            break;
        } else {
            high = count - 1;
        }
    }

    let (count, _) = best?;
    let mut bounded = response;
    bounded.values.truncate(count);
    let encoded = KrpcMessage::Response {
        transaction_id,
        response: bounded,
    }
    .encode();
    (encoded.len() <= DHT_MAX_RESPONSE_DATAGRAM_LEN).then_some(encoded)
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

fn remember_announced_peer_in_map(
    peers_by_info_hash: &mut HashMap<[u8; 20], Vec<SocketAddr>>,
    info_hash: [u8; 20],
    peer: SocketAddr,
) -> bool {
    let total = peers_by_info_hash.values().map(Vec::len).sum::<usize>();
    if let Some(peers) = peers_by_info_hash.get_mut(&info_hash) {
        // A duplicate is already represented and must remain an accepted
        // idempotent announce even when the global cache is full. New peers
        // must satisfy both the per-swarm and process-wide limits. The old
        // existing-swarm branch only checked the former, which let a single
        // known info-hash bypass DHT_ANNOUNCED_PEERS_GLOBAL_CAP entirely.
        if peers.contains(&peer) {
            return true;
        }
        if total >= DHT_ANNOUNCED_PEERS_GLOBAL_CAP {
            return false;
        }
        return remember_announced_peer(peers, peer, DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP);
    }
    if peers_by_info_hash.len() >= DHT_ANNOUNCED_PEER_SET_CAP {
        return false;
    }
    if total >= DHT_ANNOUNCED_PEERS_GLOBAL_CAP {
        return false;
    }
    peers_by_info_hash.insert(info_hash, vec![peer]);
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

    #[test]
    fn dht_ingress_budget_bounds_each_ip_and_expires_windows() {
        let now = Instant::now();
        let mut budget = DhtIngressBudget::new(now);
        let peer: SocketAddr = "192.0.2.1:6881".parse().unwrap();
        for _ in 0..DHT_INGRESS_PACKETS_PER_IP_PER_SECOND {
            assert!(budget.allow(peer, now));
        }
        assert!(!budget.allow(peer, now));
        assert!(budget.allow(peer, now + DHT_INGRESS_WINDOW));
    }

    #[test]
    fn dht_ingress_budget_has_bounded_source_state() {
        let now = Instant::now();
        let mut budget = DhtIngressBudget::new(now);
        for octet in 0..DHT_INGRESS_IP_STATE_CAP {
            let peer: SocketAddr = format!("198.{}.{}.1:6881", (octet / 256) % 256, octet % 256)
                .parse()
                .unwrap();
            let _ = budget.allow(peer, now);
        }
        assert_eq!(
            budget.per_ip.len(),
            DHT_INGRESS_GLOBAL_PACKETS_PER_SECOND as usize
        );
        assert!(budget.per_ip.len() <= DHT_INGRESS_IP_STATE_CAP);
    }

    #[tokio::test]
    async fn oversized_dht_datagram_is_observed_without_truncation() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let payload = vec![0u8; DHT_MAX_DATAGRAM_LEN + 1];
        sender
            .send_to(&payload, receiver.local_addr().unwrap())
            .await
            .unwrap();

        let mut buf = vec![0u8; DHT_MAX_DATAGRAM_LEN + 1];
        let (len, _) = receiver.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, DHT_MAX_DATAGRAM_LEN + 1);
        assert!(len > DHT_MAX_DATAGRAM_LEN);
    }

    #[test]
    fn get_peers_response_is_bounded_by_udp_payload_limit() {
        let response = DhtResponse {
            id: NodeId::from_bytes([1; 20]),
            nodes: Vec::new(),
            values: (1..=DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP)
                .map(|port| {
                    SocketAddr::V4(SocketAddrV4::new(
                        std::net::Ipv4Addr::LOCALHOST,
                        port as u16,
                    ))
                })
                .collect(),
            token: Some(vec![2; 24]),
        };
        let encoded = encode_dht_response_bounded(KrpcMessage::Response {
            transaction_id: vec![3, 4],
            response,
        })
        .expect("the response envelope must fit without peer values");

        assert!(encoded.len() <= DHT_MAX_RESPONSE_DATAGRAM_LEN);
        let KrpcMessage::Response { response, .. } = KrpcMessage::parse(&encoded).unwrap() else {
            panic!("bounded response must remain a response");
        };
        assert!(response.values.len() < DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP);
    }

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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };
        let addr: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        let response =
            task.handle_announce_peer(b"aa".to_vec(), addr, true, [9; 20], 6881, b"bad".to_vec());
        assert!(matches!(response, KrpcMessage::Error { .. }));
        assert!(task.announced_peers.is_empty());
    }

    #[tokio::test]
    async fn announce_token_does_not_depend_on_public_node_id() {
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };
        let addr: SocketAddr = "192.0.2.1:60000".parse().unwrap();
        let first = task.token_for_addr(addr);

        // The node ID is public, so changing it must not change the secret
        // portion of a token for the same source address.
        task.local_id = NodeId::from_bytes([2; 20]);
        assert_eq!(first, task.token_for_addr(addr));
    }

    #[test]
    fn token_secret_cache_recovers_from_a_clock_regression() {
        let now = Instant::now();
        let mut cache = DhtTokenSecrets {
            values: vec![([1; 20], now + Duration::from_secs(1))],
        };

        let current = cache.current(now);

        assert_eq!(current.len(), 1);
        assert_eq!(cache.values.len(), 1);
        assert_eq!(cache.values[0].0, current[0]);
        assert_eq!(cache.values[0].1, now);
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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

    #[test]
    fn announced_peer_map_is_bounded_across_info_hashes() {
        let mut peers = HashMap::new();
        for index in 0..DHT_ANNOUNCED_PEER_SET_CAP {
            let mut info_hash = [0_u8; 20];
            info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            assert!(remember_announced_peer_in_map(
                &mut peers,
                info_hash,
                "198.51.100.1:6881".parse().unwrap(),
            ));
        }
        let mut next_hash = [0_u8; 20];
        next_hash[..8].copy_from_slice(&(DHT_ANNOUNCED_PEER_SET_CAP as u64).to_be_bytes());
        assert!(!remember_announced_peer_in_map(
            &mut peers,
            next_hash,
            "198.51.100.2:6881".parse().unwrap(),
        ));
        assert_eq!(peers.len(), DHT_ANNOUNCED_PEER_SET_CAP);
    }

    #[test]
    fn announced_peer_global_cap_applies_to_existing_info_hashes() {
        let mut peers = HashMap::new();
        for index in 0..(DHT_ANNOUNCED_PEERS_GLOBAL_CAP / DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP) {
            let mut info_hash = [0_u8; 20];
            info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            for port in 0..DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP {
                assert!(remember_announced_peer_in_map(
                    &mut peers,
                    info_hash,
                    SocketAddr::from(([198, 51, 100, 1], port as u16 + 1)),
                ));
            }
        }

        let mut existing_hash = [0_u8; 20];
        existing_hash[..8].copy_from_slice(&0_u64.to_be_bytes());
        let duplicate = SocketAddr::from(([198, 51, 100, 1], 1));
        assert!(remember_announced_peer_in_map(
            &mut peers,
            existing_hash,
            duplicate,
        ));
        assert!(!remember_announced_peer_in_map(
            &mut peers,
            existing_hash,
            SocketAddr::from(([198, 51, 100, 2], 1)),
        ));
        assert_eq!(
            peers.values().map(Vec::len).sum::<usize>(),
            DHT_ANNOUNCED_PEERS_GLOBAL_CAP
        );
    }

    #[tokio::test]
    async fn pending_peer_forward_global_cap_bounds_retained_addresses() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let first_hash = [1; 20];
        let second_hash = [2; 20];
        let third_hash = [3; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let half = DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP / 2;
        let first_peers = (0..half)
            .map(|port| SocketAddr::from(([198, 51, 100, 1], port as u16)))
            .collect();
        let second_peers = (0..half)
            .map(|port| SocketAddr::from(([198, 51, 100, 2], port as u16)))
            .collect();
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
            pending_peer_forwards: HashMap::from([
                (
                    first_hash,
                    PendingPeerForward {
                        cmd_tx: cmd_tx.clone(),
                        peers: first_peers,
                    },
                ),
                (
                    second_hash,
                    PendingPeerForward {
                        cmd_tx: cmd_tx.clone(),
                        peers: second_peers,
                    },
                ),
            ]),
            generations: HashMap::new(),
        };

        assert_eq!(
            task.pending_peer_forwards
                .values()
                .map(|pending| pending.peers.len())
                .sum::<usize>(),
            DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
        );

        task.queue_pending_peer_forward(
            first_hash,
            cmd_tx.clone(),
            vec![SocketAddr::from(([198, 51, 100, 1], 60_000))],
        );
        assert_eq!(
            task.pending_peer_forwards[&first_hash].peers.len(),
            half,
            "an existing pending torrent must not bypass the global peer cap"
        );

        task.queue_pending_peer_forward(
            third_hash,
            cmd_tx,
            vec![SocketAddr::from(([198, 51, 100, 3], 60_000))],
        );
        assert!(!task.pending_peer_forwards.contains_key(&third_hash));
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
    async fn failed_get_peers_response_continues_lookup() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let info_hash = [9; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let first = std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6001);
        let second = std::net::SocketAddrV4::new("127.0.0.1".parse().unwrap(), 6002);
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
                    addr: SocketAddr::V4(first),
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([first]))]),
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };
        task.table.insert(KNode {
            id: NodeId::from_bytes([2; 20]),
            addr: first,
        });
        task.table.insert(KNode {
            id: NodeId::from_bytes([3; 20]),
            addr: second,
        });

        let error = KrpcMessage::Error {
            transaction_id: b"gp".to_vec(),
            error: DhtError {
                code: 202,
                message: "server error".to_owned(),
            },
        };
        task.handle_packet(&error.encode(), SocketAddr::V4(first))
            .await;

        assert!(!task.outstanding.contains_key(b"gp".as_slice()));
        assert!(task.queried_nodes[&info_hash].contains(&second));
        assert!(task.outstanding.values().any(|query| {
            query.addr == SocketAddr::V4(second)
                && matches!(query.request, DhtRequest::GetPeers(hash) if hash == info_hash)
        }));
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
    async fn cap_rejected_add_can_retry_after_capacity_is_freed() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let target = [255; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let mut tracked = HashMap::with_capacity(DHT_TRACKED_TORRENTS_CAP);
        for index in 0..DHT_TRACKED_TORRENTS_CAP {
            let mut info_hash = [0; 20];
            info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            tracked.insert(info_hash, cmd_tx.clone());
        }
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            torrents: tracked,
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };

        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash: target,
            cmd_tx: cmd_tx.clone(),
            generation: 1,
        }))
        .await;
        assert!(!task.torrents.contains_key(&target));
        assert!(!task.generations.contains_key(&target));

        task.torrents.remove(&[0; 20]);
        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash: target,
            cmd_tx,
            generation: 1,
        }))
        .await;

        assert!(task.torrents.contains_key(&target));
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
    async fn get_peers_response_retries_when_torrent_queue_is_full() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let remote_id = NodeId::from_bytes([2; 20]);
        let info_hash = [9; 20];
        let queried_addr: SocketAddr = "127.0.0.1:6001".parse().unwrap();
        let discovered_peer: SocketAddr = "127.0.0.1:51413".parse().unwrap();
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        let (reply, _reply_rx) = oneshot::channel();
        cmd_tx
            .try_send(TorrentCmd::GetPeers { reply })
            .expect("fill the torrent mailbox before forwarding");
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
            generations: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
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

        task.handle_packet(&response.encode(), queried_addr).await;
        assert!(task.pending_peer_forwards.contains_key(&info_hash));
        assert!(matches!(cmd_rx.try_recv(), Ok(TorrentCmd::GetPeers { .. })));

        task.flush_pending_peer_forwards();
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(TorrentCmd::NewPeers(peers)) if peers == vec![discovered_peer]
        ));
        assert!(!task.pending_peer_forwards.contains_key(&info_hash));
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
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
    async fn stale_dht_command_generation_cannot_resurrect_a_removed_torrent() {
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
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };

        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash,
            cmd_tx: cmd_tx.clone(),
            generation: 10,
        }))
        .await;
        task.handle_command(DhtCommand::RemoveTorrent {
            info_hash,
            generation: 12,
        })
        .await;
        assert!(!task.torrents.contains_key(&info_hash));

        // A retry from an older saturated-mailbox operation must not undo the
        // newer removal, even if it reaches the DHT task after that removal.
        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash,
            cmd_tx: cmd_tx.clone(),
            generation: 11,
        }))
        .await;
        assert!(!task.torrents.contains_key(&info_hash));

        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash,
            cmd_tx,
            generation: 13,
        }))
        .await;
        assert!(task.torrents.contains_key(&info_hash));
        task.handle_command(DhtCommand::RemoveTorrent {
            info_hash,
            generation: 12,
        })
        .await;
        assert!(task.torrents.contains_key(&info_hash));
    }

    #[tokio::test]
    async fn removed_torrent_does_not_receive_late_lookup_response_after_readd() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let remote_id = NodeId::from_bytes([2; 20]);
        let info_hash = [9; 20];
        let queried_addr_v4: SocketAddrV4 = "127.0.0.1:6001".parse().unwrap();
        let queried_addr = SocketAddr::V4(queried_addr_v4);
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
                b"old".to_vec(),
                OutstandingQuery {
                    addr: queried_addr,
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([queried_addr_v4]))]),
            torrents: HashMap::from([(info_hash, cmd_tx.clone())]),
            announced_peers: HashMap::new(),
            last_full_lookup: HashMap::from([(info_hash, Instant::now())]),
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };

        task.handle_command(DhtCommand::RemoveTorrent {
            info_hash,
            generation: 10,
        })
        .await;
        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash,
            cmd_tx,
            generation: 11,
        }))
        .await;

        assert!(task.outstanding.is_empty());
        let response = KrpcMessage::Response {
            transaction_id: b"old".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                values: vec![discovered_peer],
                token: None,
            },
        };
        task.handle_packet(&response.encode(), queried_addr).await;
        assert!(cmd_rx.try_recv().is_err());
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };

        task.prune_stale_outstanding();

        assert_eq!(task.outstanding.len(), 1);
        assert!(task.outstanding.contains_key(b"new".as_slice()));
    }

    #[tokio::test]
    async fn expired_get_peers_query_advances_beyond_initial_k_nodes() {
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
            pending_peer_forwards: HashMap::new(),
            generations: HashMap::new(),
        };

        for byte in 2_u8..=20 {
            task.table.insert(KNode {
                id: NodeId::from_bytes([byte; 20]),
                addr: SocketAddrV4::new("127.0.0.1".parse().unwrap(), u16::from(byte) + 6000),
            });
        }
        let initial = task
            .table
            .closest(&NodeId::from_bytes(info_hash), K)
            .into_iter()
            .map(|node| node.addr)
            .collect::<Vec<_>>();
        assert_eq!(initial.len(), K);
        let fallback = task
            .table
            .closest(
                &NodeId::from_bytes(info_hash),
                DHT_QUERIED_NODES_PER_INFO_HASH_CAP,
            )
            .into_iter()
            .find(|node| !initial.contains(&node.addr))
            .expect("routing table must contain a node beyond the initial K")
            .addr;
        task.queried_nodes
            .insert(info_hash, initial.iter().copied().collect());
        for (index, addr) in initial.iter().copied().enumerate() {
            task.outstanding.insert(
                format!("stale-{index}").into_bytes(),
                OutstandingQuery {
                    addr: SocketAddr::V4(addr),
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now() - OUTSTANDING_QUERY_TTL - Duration::from_secs(1),
                },
            );
        }

        for expired in task.prune_stale_outstanding() {
            task.continue_lookup(expired).await;
        }

        assert!(task.queried_nodes[&info_hash].contains(&fallback));
        assert!(task.outstanding.values().any(|query| {
            query.addr == SocketAddr::V4(fallback)
                && matches!(query.request, DhtRequest::GetPeers(hash) if hash == info_hash)
        }));
    }
}
