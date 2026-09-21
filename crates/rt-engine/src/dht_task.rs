//! Minimal BEP 5 DHT service loop.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use rt_dht::{
    DhtError, DhtQuery, DhtResponse, DhtWant, KNode, KNode6, KrpcMessage, NodeId, RoutingTable,
    RoutingTable6, K,
};
use sha1::{Digest, Sha1};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, timeout};
use tracing::{debug, info, warn};

use crate::command::EngineCmd;
use crate::egress_policy::OutboundEgressPolicy;
use crate::torrent_task::TorrentCmd;

const DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP: usize = 512;
const DHT_ANNOUNCED_PEER_SET_CAP: usize = 4_096;
const DHT_ANNOUNCED_PEERS_GLOBAL_CAP: usize = 16_384;
// The tracked-torrent admission cap used to be a hardcoded constant here
// (16,384), which silently dropped DHT peer discovery for any torrent beyond
// that count with no operator-visible signal beyond a single debug-level
// scan of the logs. It is now `DhtTask::tracked_torrents_cap`, sourced from
// `rt_config::DhtConfig::tracked_torrents_cap` (see `DhtRuntimeConfig`), and
// every rejection increments `DhtRuntimeStats::tracked_torrents_rejected` in
// addition to the existing `warn!` log line so the cap being hit is
// observable without grepping logs.
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
const MAX_DHT_BOOTSTRAP_NODES: usize = 256;
const MAX_DHT_BOOTSTRAP_ADDRESSES: usize = 64;
const DHT_BOOTSTRAP_DEADLINE: Duration = Duration::from_secs(10);
const DHT_BOOTSTRAP_NODE_TIMEOUT: Duration = Duration::from_secs(5);
// A failed bootstrap is retried by the periodic tick. Do not repeat the DNS
// deadline once per torrent while a large registration burst is being drained.
const DHT_BOOTSTRAP_RETRY: Duration = Duration::from_secs(30);
const DHT_ENGINE_READY_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const DHT_IPV6_COMMAND_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const DHT_IPV6_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DHT_IPV6_ABORT_GRACE: Duration = Duration::from_millis(100);

fn dht_egress_allowed(
    policy: &OutboundEgressPolicy,
    addr: SocketAddr,
    operation: &'static str,
) -> bool {
    match policy.validate_socket_addr(addr) {
        Ok(()) => true,
        Err(error) => {
            debug!(
                component = "dht",
                operation,
                peer = %addr,
                result = "rejected",
                reason = "egress_address_policy",
                error = %error,
                "DHT address rejected by outbound address policy"
            );
            false
        }
    }
}

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
    /// The configured admission cap `tracked_torrents` is measured against
    /// (`DhtConfig::tracked_torrents_cap`). Exposed alongside
    /// `tracked_torrents` so a dashboard/alert can compute saturation
    /// without hardcoding the limit.
    pub tracked_torrents_cap: u64,
    /// Cumulative count of `AddTorrent` commands rejected because
    /// `tracked_torrents` was already at `tracked_torrents_cap`. Nonzero
    /// means DHT peer discovery is unavailable for at least one torrent.
    pub tracked_torrents_rejected: u64,
    pub outstanding_requests: u64,
    pub queried_nodes: u64,
}

#[derive(Debug, Clone, Default)]
struct DhtV6Stats {
    routing_nodes: u64,
    announced_peer_sets: u64,
    announced_peers: u64,
    tracked_torrents_rejected: u64,
    outstanding_requests: u64,
    queried_nodes: u64,
}

type DhtV6StatsHandle = Arc<Mutex<DhtV6Stats>>;

enum DhtV6Command {
    AddTorrent(DhtTorrent),
    RemoveTorrent {
        info_hash: [u8; 20],
        generation: u64,
    },
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DhtRuntimeConfig {
    pub(crate) tracked_torrents_cap: usize,
    pub(crate) egress_policy: OutboundEgressPolicy,
}

pub(crate) async fn run_dht(
    port: u16,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    runtime_config: DhtRuntimeConfig,
    cmd_rx: &mut mpsc::Receiver<DhtCommand>,
    mut pending_commands: VecDeque<DhtCommand>,
    engine_tx: &mpsc::Sender<EngineCmd>,
) -> anyhow::Result<()> {
    let DhtRuntimeConfig {
        tracked_torrents_cap,
        egress_policy,
    } = runtime_config;
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
    // BEP 32 uses independent IPv4 and IPv6 DHTs. Bind IPv6 on the same
    // externally visible port when the host provides an IPv6 socket; an IPv6
    // failure is non-fatal because many container/network namespaces expose
    // only IPv4.
    let socket6 = match bind_ipv6_socket(bound.port()) {
        Ok(socket6) => {
            info!(
                component = "dht",
                operation = "listen_ipv6",
                addr = %socket6.local_addr()?,
                node_id = %local_id,
                "IPv6 DHT UDP socket bound"
            );
            Some(socket6)
        }
        Err(error) => {
            warn!(
                component = "dht",
                operation = "listen_ipv6",
                port = bound.port(),
                result = "unavailable",
                error = %error,
                "IPv6 DHT socket unavailable; continuing with IPv4 DHT"
            );
            None
        }
    };
    match timeout(
        DHT_ENGINE_READY_SEND_TIMEOUT,
        engine_tx.send(EngineCmd::DhtTaskReady),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            // The engine command receiver is gone, so there is no owner left
            // to consume DHT work or join this service. Let the supervisor
            // finish naturally after the socket is dropped.
            return Ok(());
        }
        Err(_) => {
            warn!(
                component = "dht",
                operation = "ready_notification",
                result = "timeout",
                timeout_ms = DHT_ENGINE_READY_SEND_TIMEOUT.as_millis() as u64,
                "DHT task bound its UDP socket but could not notify the engine before the deadline"
            );
        }
    }

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
        tracked_torrents_cap,
        egress_policy,
        tracked_torrents_rejected: 0,
        next_tx: random_tx_seed,
        outstanding: HashMap::new(),
        queried_nodes: HashMap::new(),
        queried_node_count: 0,
        torrents: HashMap::new(),
        generations: HashMap::new(),
        announced_peers: HashMap::new(),
        announced_peer_count: 0,
        last_full_lookup: HashMap::new(),
        pending_peer_forwards: HashMap::new(),
        pending_peer_count: 0,
        last_bootstrap_at: None,
    };
    task.bootstrap_if_due().await;

    let (ipv6_cmd_tx, ipv6_join, ipv6_stats) = if let Some(socket6) = socket6 {
        let (cmd_tx, cmd_rx) = mpsc::channel(1024);
        let stats = Arc::new(Mutex::new(DhtV6Stats::default()));
        let stats_for_task = Arc::clone(&stats);
        let bootstrap_nodes_for_task = task.bootstrap_nodes.clone();
        let join = tokio::spawn(run_ipv6_dht(
            socket6,
            local_id,
            listen_port,
            bootstrap_nodes_for_task,
            runtime_config,
            cmd_rx,
            stats_for_task,
        ));
        (Some(cmd_tx), Some(join), Some(stats))
    } else {
        (None, None, None)
    };

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
    let mut loop_error = None;
    loop {
        if let Some(cmd) = pending_commands.pop_front() {
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
            forward_ipv6_command(ipv6_cmd_tx.as_ref(), &cmd).await;
            match cmd {
                DhtCommand::GetStats { reply } => {
                    let _ = reply.send(merge_v6_stats(task.runtime_stats(), ipv6_stats.as_ref()));
                }
                cmd => {
                    if !task.handle_command(cmd).await {
                        break;
                    }
                }
            }
            continue;
        }
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
                forward_ipv6_command(ipv6_cmd_tx.as_ref(), &cmd).await;
                match cmd {
                    DhtCommand::GetStats { reply } => {
                        let _ = reply.send(merge_v6_stats(task.runtime_stats(), ipv6_stats.as_ref()));
                    }
                    cmd => {
                        if !task.handle_command(cmd).await {
                            break;
                        }
                    }
                }
            }
            _ = bootstrap_tick.tick() => {
                if task.table.total_nodes() < K {
                    task.bootstrap_if_due().await;
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
                task.prune_closed_torrents();
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
                    Err(e) => {
                        loop_error = Some(anyhow::Error::new(e));
                        break;
                    }
                }
            }
        }
    }
    if let Some(cmd_tx) = ipv6_cmd_tx {
        match timeout(
            DHT_IPV6_COMMAND_SEND_TIMEOUT,
            cmd_tx.send(DhtV6Command::Shutdown),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {}
            Err(_) => {
                warn!(
                    component = "dht",
                    operation = "shutdown_ipv6",
                    result = "command_timeout",
                    timeout_ms = DHT_IPV6_COMMAND_SEND_TIMEOUT.as_millis() as u64,
                    "IPv6 DHT shutdown command could not be queued before the deadline"
                );
            }
        }
    }
    if let Some(mut join) = ipv6_join {
        if let Some(result) = crate::shutdown_join_task(
            &mut join,
            DHT_IPV6_SHUTDOWN_TIMEOUT,
            DHT_IPV6_ABORT_GRACE,
            "dht",
            "shutdown_ipv6",
            "IPv6 DHT task",
            || {},
        )
        .await
        {
            if result.is_err() {
                warn!(
                    component = "dht",
                    operation = "shutdown_ipv6",
                    result = "task_error",
                    "IPv6 DHT task returned an error during shutdown"
                );
            }
        }
    }
    // Drop the socket-owning task before acknowledging shutdown. Callers can
    // then safely bind the configured DHT port for a replacement engine.
    drop(task);
    if let Some(reply) = shutdown_reply {
        let _ = reply.send(());
    }
    loop_error.map_or(Ok(()), Err)
}

struct DhtTask {
    local_id: NodeId,
    table: RoutingTable,
    socket: UdpSocket,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    /// Maximum number of torrents concurrently tracked for DHT peer
    /// discovery. Sourced from `rt_config::DhtConfig::tracked_torrents_cap`;
    /// see the comment above `DHT_QUERIED_NODES_PER_INFO_HASH_CAP` for why
    /// this moved from a hardcoded constant to a runtime config value.
    tracked_torrents_cap: usize,
    egress_policy: OutboundEgressPolicy,
    /// Cumulative `AddTorrent` rejections caused by `tracked_torrents_cap`.
    /// Surfaced via `runtime_stats()` -> `DhtRuntimeStats::tracked_torrents_rejected`.
    tracked_torrents_rejected: u64,
    next_tx: u16,
    outstanding: HashMap<Vec<u8>, OutstandingQuery>,
    queried_nodes: HashMap<[u8; 20], HashSet<SocketAddrV4>>,
    /// Aggregate count for queried-node history. Lookup admission and stats
    /// must not rescan every tracked torrent on each query.
    queried_node_count: usize,
    torrents: HashMap<[u8; 20], mpsc::Sender<TorrentCmd>>,
    /// Last accepted engine command generation, including removed torrents.
    /// Retaining bounded tombstones prevents a delayed Add from resurrecting
    /// a torrent after a later Remove was delivered first.
    generations: HashMap<[u8; 20], u64>,
    announced_peers: HashMap<[u8; 20], Vec<SocketAddr>>,
    /// Aggregate count for the announced-peer cache. Keeping this alongside
    /// the map avoids rescanning every peer list for each announce_peer.
    announced_peer_count: usize,
    last_full_lookup: HashMap<[u8; 20], Instant>,
    pending_peer_forwards: HashMap<[u8; 20], PendingPeerForward>,
    /// Aggregate count for peer batches waiting on torrent mailboxes. This
    /// keeps DHT retry admission O(1) instead of recounting every stalled
    /// torrent on each response.
    pending_peer_count: usize,
    last_bootstrap_at: Option<Instant>,
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
    fn bootstrap_attempt_is_due(last_attempt: Option<Instant>, now: Instant) -> bool {
        !last_attempt.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_some_and(|elapsed| elapsed < DHT_BOOTSTRAP_RETRY)
        })
    }

    fn accept_generation(&mut self, info_hash: [u8; 20], generation: u64) -> bool {
        if self
            .generations
            .get(&info_hash)
            .is_some_and(|current| *current >= generation)
        {
            return false;
        }
        if !self.generations.contains_key(&info_hash)
            && self.generations.len() >= self.tracked_torrents_cap.saturating_mul(2)
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
        if let Some(nodes) = self.queried_nodes.remove(&info_hash) {
            self.queried_node_count = self.queried_node_count.saturating_sub(nodes.len());
        }
        self.last_full_lookup.remove(&info_hash);
    }

    /// A removal command can be discarded when the bounded retry budget is
    /// exhausted. The torrent task still owns the receiving half of its
    /// mailbox until shutdown completes, so the DHT sender becomes closed
    /// eventually even in that case. Reconcile those registrations here so a
    /// dropped removal cannot leave lookup, announce, or pending-forward state
    /// behind forever.
    fn remove_torrent_state(&mut self, info_hash: [u8; 20]) {
        self.torrents.remove(&info_hash);
        self.clear_torrent_lookup_state(info_hash);
        if let Some(peers) = self.announced_peers.remove(&info_hash) {
            self.announced_peer_count = self.announced_peer_count.saturating_sub(peers.len());
        }
        if let Some(pending) = self.pending_peer_forwards.remove(&info_hash) {
            self.pending_peer_count = self.pending_peer_count.saturating_sub(pending.peers.len());
        }
    }

    fn prune_closed_torrents(&mut self) {
        let closed = self
            .torrents
            .iter()
            .filter_map(|(info_hash, cmd_tx)| cmd_tx.is_closed().then_some(*info_hash))
            .collect::<Vec<_>>();
        for info_hash in closed {
            self.remove_torrent_state(info_hash);
        }
    }

    async fn handle_command(&mut self, cmd: DhtCommand) -> bool {
        match cmd {
            DhtCommand::AddTorrent(torrent) => {
                if !self.torrents.contains_key(&torrent.info_hash)
                    && self.torrents.len() >= self.tracked_torrents_cap
                {
                    self.tracked_torrents_rejected =
                        self.tracked_torrents_rejected.saturating_add(1);
                    warn!(
                        component = "dht",
                        operation = "track_torrent",
                        result = "rejected",
                        reason = "tracked torrent cap exceeded",
                        cap = self.tracked_torrents_cap,
                        rejected_total = self.tracked_torrents_rejected,
                        "DHT tracking admission cap exceeded; this torrent will not receive DHT-discovered peers"
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
                if let Some(pending) = self.pending_peer_forwards.remove(&torrent.info_hash) {
                    self.pending_peer_count =
                        self.pending_peer_count.saturating_sub(pending.peers.len());
                }
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
                self.remove_torrent_state(info_hash);
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
        let node_count = self.bootstrap_nodes.len().min(MAX_DHT_BOOTSTRAP_NODES);
        let deadline = Instant::now() + DHT_BOOTSTRAP_DEADLINE;
        for node_index in 0..node_count {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                warn!(
                    component = "dht",
                    operation = "bootstrap_resolve",
                    result = "deadline_exceeded",
                    deadline_secs = DHT_BOOTSTRAP_DEADLINE.as_secs(),
                    "DHT bootstrap deadline exceeded"
                );
                break;
            };
            if remaining.is_zero() {
                break;
            }
            let node = self.bootstrap_nodes[node_index].clone();
            let resolve_timeout = remaining.min(DHT_BOOTSTRAP_NODE_TIMEOUT);
            let addrs =
                match tokio::time::timeout(resolve_timeout, tokio::net::lookup_host(&node)).await {
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
            let addrs = addrs
                .take(MAX_DHT_BOOTSTRAP_ADDRESSES.saturating_add(1))
                .collect::<Vec<_>>();
            if addrs.len() > MAX_DHT_BOOTSTRAP_ADDRESSES {
                warn!(
                    component = "dht",
                    operation = "bootstrap_resolve",
                    node = %node,
                    result = "rejected",
                    address_cap = MAX_DHT_BOOTSTRAP_ADDRESSES,
                    "DHT bootstrap returned too many addresses"
                );
                continue;
            }
            for addr in addrs {
                if deadline.checked_duration_since(Instant::now()).is_none() {
                    break;
                }
                if !addr.is_ipv4()
                    || !dht_egress_allowed(&self.egress_policy, addr, "bootstrap_query")
                {
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

    async fn bootstrap_if_due(&mut self) {
        let now = Instant::now();
        if !Self::bootstrap_attempt_is_due(self.last_bootstrap_at, now) {
            return;
        }
        // Record the attempt before resolving. This is deliberately a
        // cooldown, not an in-flight flag: the DHT actor is single-threaded,
        // so no second bootstrap can overlap this one, while repeated Add
        // commands must not each pay the same failed DNS deadline.
        self.last_bootstrap_at = Some(now);
        self.bootstrap().await;
    }

    async fn handle_packet(&mut self, packet: &[u8], addr: SocketAddr) {
        if !dht_egress_allowed(&self.egress_policy, addr, "receive_packet") {
            return;
        }
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
                    if dht_egress_allowed(
                        &self.egress_policy,
                        SocketAddr::V4(node.addr),
                        "learn_node",
                    ) {
                        self.table.insert(node);
                    }
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
        if !dht_egress_allowed(&self.egress_policy, addr, "send_response") {
            return;
        }
        let response = match query {
            DhtQuery::Ping { .. } => KrpcMessage::Response {
                transaction_id,
                response: DhtResponse::new(self.local_id),
            },
            DhtQuery::FindNode { target, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.closest_response(target),
            },
            DhtQuery::FindNodeWithWant { target, want, .. } => KrpcMessage::Response {
                transaction_id,
                response: if want.contains(&DhtWant::Ipv4) {
                    self.closest_response(target)
                } else {
                    DhtResponse::new(self.local_id)
                },
            },
            DhtQuery::GetPeers { info_hash, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.get_peers_response(info_hash, addr),
            },
            DhtQuery::GetPeersWithWant {
                info_hash, want, ..
            } => KrpcMessage::Response {
                transaction_id,
                response: if want.contains(&DhtWant::Ipv4) {
                    self.get_peers_response(info_hash, addr)
                } else {
                    let mut response = DhtResponse::new(self.local_id);
                    response.token = Some(self.token_for_addr(addr));
                    response
                },
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
        remember_announced_peer_in_map(
            &mut self.announced_peers,
            &mut self.announced_peer_count,
            info_hash,
            peer,
        );
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
            self.bootstrap_if_due().await;
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
            if let Some(nodes) = self.queried_nodes.remove(&info_hash) {
                self.queried_node_count = self.queried_node_count.saturating_sub(nodes.len());
            }
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
        DHT_QUERIED_NODES_GLOBAL_CAP.saturating_sub(self.queried_node_count)
    }

    async fn send_get_peers(
        &mut self,
        info_hash: [u8; 20],
        addr: SocketAddr,
        query_budget: usize,
    ) -> bool {
        if !dht_egress_allowed(&self.egress_policy, addr, "send_get_peers") {
            return false;
        }
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
        self.queried_node_count = self.queried_node_count.saturating_add(1);
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
            if let Some(queried) = self.queried_nodes.get_mut(&info_hash) {
                if queried.remove(&v4) {
                    self.queried_node_count = self.queried_node_count.saturating_sub(1);
                }
                if queried.is_empty() {
                    self.queried_nodes.remove(&info_hash);
                }
            }
            return false;
        }
        if let Err(e) = self.socket.send_to(&msg.encode(), addr).await {
            self.outstanding.remove(&tx);
            if let Some(queried) = self.queried_nodes.get_mut(&info_hash) {
                if queried.remove(&v4) {
                    self.queried_node_count = self.queried_node_count.saturating_sub(1);
                }
                if queried.is_empty() {
                    self.queried_nodes.remove(&info_hash);
                }
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
        if !dht_egress_allowed(&self.egress_policy, addr, "send_announce_peer") {
            return;
        }
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
        let peers = peers
            .into_iter()
            .filter(|peer| dht_egress_allowed(&self.egress_policy, *peer, "forward_peer"))
            .collect::<Vec<_>>();
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
                self.remove_torrent_state(info_hash);
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
        if !self.pending_peer_forwards.contains_key(&info_hash)
            && self.pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
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
            if self.pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP {
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
            self.pending_peer_count = self.pending_peer_count.saturating_add(1);
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
        self.pending_peer_count = self.pending_peer_count.saturating_sub(pending.peers.len());
        match pending.cmd_tx.try_send(TorrentCmd::NewPeers(pending.peers)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(TorrentCmd::NewPeers(peers))) => {
                self.pending_peer_count = self.pending_peer_count.saturating_add(peers.len());
                self.pending_peer_forwards.insert(
                    info_hash,
                    PendingPeerForward {
                        cmd_tx: pending.cmd_tx,
                        peers,
                    },
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.remove_torrent_state(info_hash);
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
            | DhtQuery::FindNodeWithWant { id, .. }
            | DhtQuery::GetPeers { id, .. }
            | DhtQuery::GetPeersWithWant { id, .. }
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
            announced_peers: self.announced_peer_count as u64,
            tracked_torrents: self.torrents.len() as u64,
            tracked_torrents_cap: self.tracked_torrents_cap as u64,
            tracked_torrents_rejected: self.tracked_torrents_rejected,
            outstanding_requests: self.outstanding.len() as u64,
            queried_nodes: self.queried_node_count as u64,
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
        // Keep timed-out addresses in `queried_nodes` for this lookup round.
        // The continuation must advance to other routing-table candidates
        // instead of immediately selecting the same silent K nodes again.
        // The bounded history is cleared on the next full lookup restart.
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

fn bind_ipv6_socket(port: u16) -> std::io::Result<UdpSocket> {
    let socket = std::net::UdpSocket::bind((Ipv6Addr::UNSPECIFIED, port))?;
    #[cfg(unix)]
    {
        let value: libc::c_int = 1;
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::IPPROTO_IPV6,
                libc::IPV6_V6ONLY,
                (&value as *const libc::c_int).cast(),
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket)
}

async fn forward_ipv6_command(cmd_tx: Option<&mpsc::Sender<DhtV6Command>>, cmd: &DhtCommand) {
    let Some(cmd_tx) = cmd_tx else {
        return;
    };
    let command = match cmd {
        DhtCommand::AddTorrent(torrent) => Some(DhtV6Command::AddTorrent(torrent.clone())),
        DhtCommand::RemoveTorrent {
            info_hash,
            generation,
        } => Some(DhtV6Command::RemoveTorrent {
            info_hash: *info_hash,
            generation: *generation,
        }),
        DhtCommand::GetStats { .. } | DhtCommand::Shutdown { .. } => None,
    };
    if let Some(command) = command {
        match timeout(DHT_IPV6_COMMAND_SEND_TIMEOUT, cmd_tx.send(command)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                warn!(
                    component = "dht",
                    operation = "forward_ipv6_command",
                    result = "closed",
                    "IPv6 DHT command channel is closed"
                );
            }
            Err(_) => {
                warn!(
                    component = "dht",
                    operation = "forward_ipv6_command",
                    result = "timeout",
                    timeout_ms = DHT_IPV6_COMMAND_SEND_TIMEOUT.as_millis() as u64,
                    "IPv6 DHT command channel remained full; dropping this mirrored command"
                );
            }
        }
    }
}

fn merge_v6_stats(mut stats: DhtRuntimeStats, ipv6: Option<&DhtV6StatsHandle>) -> DhtRuntimeStats {
    let Some(ipv6) = ipv6 else {
        return stats;
    };
    let Ok(ipv6) = ipv6.lock() else {
        return stats;
    };
    stats.routing_nodes = stats.routing_nodes.saturating_add(ipv6.routing_nodes);
    stats.announced_peer_sets = stats
        .announced_peer_sets
        .saturating_add(ipv6.announced_peer_sets);
    stats.announced_peers = stats.announced_peers.saturating_add(ipv6.announced_peers);
    stats.tracked_torrents_rejected = stats
        .tracked_torrents_rejected
        .saturating_add(ipv6.tracked_torrents_rejected);
    stats.outstanding_requests = stats
        .outstanding_requests
        .saturating_add(ipv6.outstanding_requests);
    stats.queried_nodes = stats.queried_nodes.saturating_add(ipv6.queried_nodes);
    stats
}

async fn run_ipv6_dht(
    socket: UdpSocket,
    local_id: NodeId,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    runtime_config: DhtRuntimeConfig,
    cmd_rx: mpsc::Receiver<DhtV6Command>,
    stats: DhtV6StatsHandle,
) -> anyhow::Result<()> {
    let DhtRuntimeConfig {
        tracked_torrents_cap,
        egress_policy,
    } = runtime_config;
    let mut task = DhtV6Task {
        local_id,
        table: RoutingTable6::new(local_id),
        socket,
        listen_port,
        bootstrap_nodes,
        tracked_torrents_cap,
        egress_policy,
        tracked_torrents_rejected: 0,
        next_tx: {
            let seed = *NodeId::random().as_bytes();
            u16::from_be_bytes([seed[2], seed[3]]).max(1)
        },
        outstanding: HashMap::new(),
        queried_nodes: HashMap::new(),
        queried_node_count: 0,
        torrents: HashMap::new(),
        generations: HashMap::new(),
        announced_peers: HashMap::new(),
        announced_peer_count: 0,
        last_full_lookup: HashMap::new(),
        pending_peer_forwards: HashMap::new(),
        pending_peer_count: 0,
        last_bootstrap_at: None,
        stats,
    };
    task.bootstrap_if_due().await;

    let mut cmd_rx = cmd_rx;
    let mut bootstrap_tick = interval(Duration::from_secs(300));
    let mut search_tick = interval(Duration::from_secs(30));
    let mut outstanding_sweep_tick = interval(Duration::from_secs(10));
    let mut pending_forward_tick = interval(Duration::from_millis(100));
    let mut ingress_budget = DhtIngressBudget::new(Instant::now());
    let mut buf = vec![0u8; DHT_MAX_DATAGRAM_LEN.saturating_add(1)];

    loop {
        tokio::select! {
            command = cmd_rx.recv() => {
                match command {
                    Some(DhtV6Command::AddTorrent(torrent)) => {
                        task.handle_command(DhtV6Command::AddTorrent(torrent)).await;
                    }
                    Some(DhtV6Command::RemoveTorrent { info_hash, generation }) => {
                        task.handle_command(DhtV6Command::RemoveTorrent { info_hash, generation }).await;
                    }
                    Some(DhtV6Command::Shutdown) | None => break,
                }
            }
            _ = bootstrap_tick.tick() => {
                if task.table.total_nodes() < K {
                    task.bootstrap_if_due().await;
                }
            }
            _ = search_tick.tick() => task.search_torrents().await,
            _ = outstanding_sweep_tick.tick() => {
                for info_hash in task.prune_stale_outstanding() {
                    task.continue_lookup(info_hash).await;
                }
            }
            _ = pending_forward_tick.tick() => {
                task.prune_closed_torrents();
                task.flush_pending_peer_forwards();
            }
            recv = task.socket.recv_from(&mut buf) => {
                match recv {
                    Ok((n, addr)) => {
                        if !ingress_budget.allow(addr, Instant::now()) {
                            debug!(component = "dht", operation = "ingress_rate_limit", peer = %addr, result = "rejected", "IPv6 DHT packet rate limit exceeded");
                        } else if n > DHT_MAX_DATAGRAM_LEN {
                            debug!(component = "dht", operation = "ingress_size_limit", peer = %addr, bytes = n, result = "rejected", "IPv6 DHT datagram exceeds configured parser limit");
                        } else if addr.is_ipv6() {
                            task.handle_packet(&buf[..n], addr).await;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error).context("receiving IPv6 DHT UDP datagram"),
                }
            }
        }
    }
    Ok(())
}

struct DhtV6Task {
    local_id: NodeId,
    table: RoutingTable6,
    socket: UdpSocket,
    listen_port: u16,
    bootstrap_nodes: Vec<String>,
    tracked_torrents_cap: usize,
    egress_policy: OutboundEgressPolicy,
    tracked_torrents_rejected: u64,
    next_tx: u16,
    outstanding: HashMap<Vec<u8>, OutstandingQuery>,
    queried_nodes: HashMap<[u8; 20], HashSet<SocketAddrV6>>,
    queried_node_count: usize,
    torrents: HashMap<[u8; 20], mpsc::Sender<TorrentCmd>>,
    generations: HashMap<[u8; 20], u64>,
    announced_peers: HashMap<[u8; 20], Vec<SocketAddr>>,
    announced_peer_count: usize,
    last_full_lookup: HashMap<[u8; 20], Instant>,
    pending_peer_forwards: HashMap<[u8; 20], PendingPeerForward>,
    pending_peer_count: usize,
    last_bootstrap_at: Option<Instant>,
    stats: DhtV6StatsHandle,
}

impl DhtV6Task {
    /// Keep removed torrent generations as bounded tombstones, matching the
    /// IPv4 DHT actor. Without this cap, a long-running daemon accumulates one
    /// hash entry for every torrent incarnation ever seen on IPv6.
    fn accept_generation(&mut self, info_hash: [u8; 20], generation: u64) -> bool {
        if self
            .generations
            .get(&info_hash)
            .is_some_and(|current| *current >= generation)
        {
            return false;
        }
        if !self.generations.contains_key(&info_hash)
            && self.generations.len() >= self.tracked_torrents_cap.saturating_mul(2)
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

    async fn handle_command(&mut self, command: DhtV6Command) {
        match command {
            DhtV6Command::AddTorrent(torrent) => {
                if !self.torrents.contains_key(&torrent.info_hash)
                    && self.torrents.len() >= self.tracked_torrents_cap
                {
                    self.tracked_torrents_rejected =
                        self.tracked_torrents_rejected.saturating_add(1);
                    self.update_stats();
                    return;
                }
                if !self.accept_generation(torrent.info_hash, torrent.generation) {
                    self.update_stats();
                    return;
                }
                self.remove_lookup_state(torrent.info_hash);
                self.torrents.insert(torrent.info_hash, torrent.cmd_tx);
                self.search_torrent(torrent.info_hash, true).await;
            }
            DhtV6Command::RemoveTorrent {
                info_hash,
                generation,
            } => {
                if !self.accept_generation(info_hash, generation) {
                    self.update_stats();
                    return;
                }
                self.remove_torrent(info_hash);
            }
            DhtV6Command::Shutdown => {}
        }
        self.update_stats();
    }

    fn remove_lookup_state(&mut self, info_hash: [u8; 20]) {
        self.outstanding.retain(|_, query| {
            !matches!(query.request, DhtRequest::GetPeers(candidate) if candidate == info_hash)
        });
        if let Some(nodes) = self.queried_nodes.remove(&info_hash) {
            self.queried_node_count = self.queried_node_count.saturating_sub(nodes.len());
        }
        self.last_full_lookup.remove(&info_hash);
        if let Some(pending) = self.pending_peer_forwards.remove(&info_hash) {
            self.pending_peer_count = self.pending_peer_count.saturating_sub(pending.peers.len());
        }
    }

    fn remove_torrent(&mut self, info_hash: [u8; 20]) {
        self.torrents.remove(&info_hash);
        self.remove_lookup_state(info_hash);
        if let Some(peers) = self.announced_peers.remove(&info_hash) {
            self.announced_peer_count = self.announced_peer_count.saturating_sub(peers.len());
        }
    }

    fn prune_closed_torrents(&mut self) {
        let closed = self
            .torrents
            .iter()
            .filter_map(|(hash, tx)| tx.is_closed().then_some(*hash))
            .collect::<Vec<_>>();
        for info_hash in closed {
            self.remove_torrent(info_hash);
        }
        self.update_stats();
    }

    async fn bootstrap(&mut self) {
        let node_count = self.bootstrap_nodes.len().min(MAX_DHT_BOOTSTRAP_NODES);
        let deadline = Instant::now() + DHT_BOOTSTRAP_DEADLINE;
        let bootstrap_nodes = self
            .bootstrap_nodes
            .iter()
            .take(node_count)
            .cloned()
            .collect::<Vec<_>>();
        for node in bootstrap_nodes {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let addrs = match timeout(
                remaining.min(DHT_BOOTSTRAP_NODE_TIMEOUT),
                tokio::net::lookup_host(&node),
            )
            .await
            {
                Ok(Ok(addrs)) => addrs
                    .filter(SocketAddr::is_ipv6)
                    .take(MAX_DHT_BOOTSTRAP_ADDRESSES.saturating_add(1))
                    .collect::<Vec<_>>(),
                _ => continue,
            };
            for addr in addrs.into_iter().take(MAX_DHT_BOOTSTRAP_ADDRESSES) {
                if !dht_egress_allowed(&self.egress_policy, addr, "bootstrap_query_ipv6") {
                    continue;
                }
                let tx = self.transaction_id();
                let message = KrpcMessage::Query {
                    transaction_id: tx.clone(),
                    query: DhtQuery::FindNodeWithWant {
                        id: self.local_id,
                        target: self.local_id,
                        want: vec![DhtWant::Ipv6],
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
                if self.socket.send_to(&message.encode(), addr).await.is_err() {
                    self.outstanding.remove(&tx);
                }
            }
        }
    }

    async fn bootstrap_if_due(&mut self) {
        let now = Instant::now();
        if !DhtTask::bootstrap_attempt_is_due(self.last_bootstrap_at, now) {
            return;
        }
        self.last_bootstrap_at = Some(now);
        self.bootstrap().await;
    }

    async fn handle_packet(&mut self, packet: &[u8], addr: SocketAddr) {
        if !dht_egress_allowed(&self.egress_policy, addr, "receive_packet_ipv6") {
            return;
        }
        let Ok(message) = KrpcMessage::parse(packet) else {
            return;
        };
        match message {
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
                let Some(outstanding) = self.outstanding.get(&transaction_id).copied() else {
                    return;
                };
                if outstanding.addr != addr || !addr.is_ipv6() {
                    return;
                }
                self.outstanding.remove(&transaction_id);
                self.remember_node(response.id, addr);
                for node in response.nodes6 {
                    if dht_egress_allowed(
                        &self.egress_policy,
                        SocketAddr::V6(node.addr),
                        "learn_node_ipv6",
                    ) {
                        self.table.insert(node);
                    }
                }
                if let DhtRequest::GetPeers(info_hash) = outstanding.request {
                    if let Some(token) = response.token {
                        self.announce_peer_to_node(info_hash, token, addr).await;
                    }
                    self.forward_peers(
                        info_hash,
                        response
                            .values
                            .into_iter()
                            .filter(SocketAddr::is_ipv6)
                            .collect(),
                    );
                    self.continue_lookup(info_hash).await;
                }
            }
            KrpcMessage::Error { transaction_id, .. } => {
                let Some(outstanding) = self.outstanding.get(&transaction_id).copied() else {
                    return;
                };
                if outstanding.addr != addr {
                    return;
                }
                self.outstanding.remove(&transaction_id);
                if let DhtRequest::GetPeers(info_hash) = outstanding.request {
                    self.continue_lookup(info_hash).await;
                }
            }
        }
        self.update_stats();
    }

    async fn handle_query(&mut self, transaction_id: Vec<u8>, query: DhtQuery, addr: SocketAddr) {
        if !dht_egress_allowed(&self.egress_policy, addr, "send_response_ipv6") {
            return;
        }
        let response = match query {
            DhtQuery::Ping { .. } => KrpcMessage::Response {
                transaction_id,
                response: DhtResponse::new(self.local_id),
            },
            DhtQuery::FindNode { target, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.closest_response(target, true),
            },
            DhtQuery::FindNodeWithWant { target, want, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.closest_response(target, want.contains(&DhtWant::Ipv6)),
            },
            DhtQuery::GetPeers { info_hash, .. } => KrpcMessage::Response {
                transaction_id,
                response: self.get_peers_response(info_hash, addr, true),
            },
            DhtQuery::GetPeersWithWant {
                info_hash, want, ..
            } => KrpcMessage::Response {
                transaction_id,
                response: self.get_peers_response(info_hash, addr, want.contains(&DhtWant::Ipv6)),
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
            return;
        };
        let _ = self.socket.send_to(&encoded, addr).await;
    }

    fn closest_response(&self, target: NodeId, include_v6: bool) -> DhtResponse {
        let mut response = DhtResponse::new(self.local_id);
        if include_v6 {
            response.nodes6 = self
                .table
                .closest(&target, K)
                .into_iter()
                .cloned()
                .collect();
        }
        response
    }

    fn get_peers_response(
        &self,
        info_hash: [u8; 20],
        addr: SocketAddr,
        include_v6: bool,
    ) -> DhtResponse {
        let mut response = self.closest_response(NodeId::from_bytes(info_hash), include_v6);
        response.token = Some(self.token_for_addr(addr));
        if include_v6 {
            if let Some(peers) = self.announced_peers.get(&info_hash) {
                response.values = peers.iter().copied().filter(SocketAddr::is_ipv6).collect();
                if !response.values.is_empty() {
                    response.nodes6.clear();
                }
            }
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
        if !addr.is_ipv6()
            || !dht_token_secrets(Instant::now())
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
        remember_announced_peer_in_map(
            &mut self.announced_peers,
            &mut self.announced_peer_count,
            info_hash,
            peer,
        );
        KrpcMessage::Response {
            transaction_id,
            response: DhtResponse::new(self.local_id),
        }
    }

    fn token_for_addr(&self, addr: SocketAddr) -> Vec<u8> {
        let secret = dht_token_secrets(Instant::now())
            .into_iter()
            .next()
            .expect("DHT token secret cache always has a current secret");
        dht_token_for_secret(addr, &secret)
    }

    async fn search_torrents(&mut self) {
        let mut budget = DHT_QUERIED_NODES_GLOBAL_CAP;
        for info_hash in self.torrents.keys().copied().collect::<Vec<_>>() {
            if budget == 0 {
                break;
            }
            budget = self
                .search_torrent_with_budget(info_hash, false, budget)
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
        mut budget: usize,
    ) -> usize {
        self.maybe_restart_lookup(info_hash, force_restart);
        budget = budget.min(DHT_QUERIED_NODES_GLOBAL_CAP.saturating_sub(self.queried_node_count));
        let nodes = self
            .table
            .closest(&NodeId::from_bytes(info_hash), K)
            .into_iter()
            .map(|node| node.addr)
            .collect::<Vec<_>>();
        if nodes.is_empty() {
            self.bootstrap_if_due().await;
            return budget;
        }
        for addr in nodes {
            if budget == 0 {
                break;
            }
            if self.send_get_peers(info_hash, addr).await {
                budget -= 1;
            }
        }
        budget
    }

    fn maybe_restart_lookup(&mut self, info_hash: [u8; 20], force_restart: bool) {
        const RESTART_AFTER: Duration = Duration::from_secs(120);
        let now = Instant::now();
        if force_restart
            || self
                .last_full_lookup
                .get(&info_hash)
                .is_none_or(|last| now.duration_since(*last) >= RESTART_AFTER)
        {
            if let Some(nodes) = self.queried_nodes.remove(&info_hash) {
                self.queried_node_count = self.queried_node_count.saturating_sub(nodes.len());
            }
            self.last_full_lookup.insert(info_hash, now);
        }
    }

    async fn continue_lookup(&mut self, info_hash: [u8; 20]) {
        if !self.torrents.contains_key(&info_hash) {
            return;
        }
        let mut budget = DHT_QUERIED_NODES_GLOBAL_CAP.saturating_sub(self.queried_node_count);
        let addrs = self
            .table
            .closest(
                &NodeId::from_bytes(info_hash),
                DHT_QUERIED_NODES_PER_INFO_HASH_CAP,
            )
            .into_iter()
            .filter_map(|node| {
                let already = self
                    .queried_nodes
                    .get(&info_hash)
                    .is_some_and(|nodes| nodes.contains(&node.addr));
                (!already).then_some(node.addr)
            })
            .collect::<Vec<_>>();
        for addr in addrs.into_iter().take(K) {
            if budget == 0 {
                break;
            }
            if self.send_get_peers(info_hash, addr).await {
                budget -= 1;
            }
        }
    }

    async fn send_get_peers(&mut self, info_hash: [u8; 20], addr: SocketAddrV6) -> bool {
        if !dht_egress_allowed(
            &self.egress_policy,
            SocketAddr::V6(addr),
            "send_get_peers_ipv6",
        ) {
            return false;
        }
        if self.outstanding.len() >= DHT_OUTSTANDING_QUERY_CAP
            || self
                .queried_nodes
                .get(&info_hash)
                .is_some_and(|nodes| nodes.len() >= DHT_QUERIED_NODES_PER_INFO_HASH_CAP)
        {
            return false;
        }
        if !self
            .queried_nodes
            .entry(info_hash)
            .or_default()
            .insert(addr)
        {
            return false;
        }
        self.queried_node_count = self.queried_node_count.saturating_add(1);
        let tx = self.transaction_id();
        let message = KrpcMessage::Query {
            transaction_id: tx.clone(),
            query: DhtQuery::GetPeersWithWant {
                id: self.local_id,
                info_hash,
                want: vec![DhtWant::Ipv6],
            },
        };
        if !self.insert_outstanding(
            tx.clone(),
            OutstandingQuery {
                addr: SocketAddr::V6(addr),
                request: DhtRequest::GetPeers(info_hash),
                sent_at: Instant::now(),
            },
        ) {
            self.remove_queried(info_hash, addr);
            return false;
        }
        if self.socket.send_to(&message.encode(), addr).await.is_err() {
            self.outstanding.remove(&tx);
            self.remove_queried(info_hash, addr);
            return false;
        }
        true
    }

    fn remove_queried(&mut self, info_hash: [u8; 20], addr: SocketAddrV6) {
        if let Some(nodes) = self.queried_nodes.get_mut(&info_hash) {
            if nodes.remove(&addr) {
                self.queried_node_count = self.queried_node_count.saturating_sub(1);
            }
            if nodes.is_empty() {
                self.queried_nodes.remove(&info_hash);
            }
        }
    }

    async fn announce_peer_to_node(
        &mut self,
        info_hash: [u8; 20],
        token: Vec<u8>,
        addr: SocketAddr,
    ) {
        if !dht_egress_allowed(&self.egress_policy, addr, "send_announce_peer_ipv6") {
            return;
        }
        let tx = self.transaction_id();
        let message = KrpcMessage::Query {
            transaction_id: tx.clone(),
            query: DhtQuery::AnnouncePeer {
                id: self.local_id,
                implied_port: false,
                info_hash,
                port: self.listen_port,
                token,
            },
        };
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
        if self.socket.send_to(&message.encode(), addr).await.is_err() {
            self.outstanding.remove(&tx);
        }
    }

    fn forward_peers(&mut self, info_hash: [u8; 20], peers: Vec<SocketAddr>) {
        let peers = peers
            .into_iter()
            .filter(SocketAddr::is_ipv6)
            .filter(|peer| dht_egress_allowed(&self.egress_policy, *peer, "forward_peer_ipv6"))
            .collect::<Vec<_>>();
        if peers.is_empty() {
            return;
        }
        let Some(cmd_tx) = self.torrents.get(&info_hash).cloned() else {
            return;
        };
        if self.pending_peer_forwards.contains_key(&info_hash) {
            self.queue_pending_peer_forward(info_hash, cmd_tx, peers);
            self.flush_pending_peer_forward(info_hash);
            return;
        }
        match cmd_tx.try_send(TorrentCmd::NewPeers(peers)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => self.remove_torrent(info_hash),
            Err(mpsc::error::TrySendError::Full(TorrentCmd::NewPeers(peers))) => {
                self.queue_pending_peer_forward(info_hash, cmd_tx, peers)
            }
            Err(mpsc::error::TrySendError::Full(_)) => unreachable!(),
        }
    }

    fn queue_pending_peer_forward(
        &mut self,
        info_hash: [u8; 20],
        cmd_tx: mpsc::Sender<TorrentCmd>,
        peers: Vec<SocketAddr>,
    ) {
        if !self.pending_peer_forwards.contains_key(&info_hash)
            && self.pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
        {
            debug!(
                component = "dht",
                operation = "forward_peers_ipv6",
                result = "pending_peer_global_cap_exceeded",
                cap = DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP,
                "dropping IPv6 DHT peers because the global pending-forward peer cap is full"
            );
            return;
        }
        if !self.pending_peer_forwards.contains_key(&info_hash)
            && self.pending_peer_forwards.len() >= DHT_PENDING_FORWARD_TORRENT_CAP
        {
            debug!(
                component = "dht",
                operation = "forward_peers_ipv6",
                result = "pending_cap_exceeded",
                cap = DHT_PENDING_FORWARD_TORRENT_CAP,
                "dropping IPv6 DHT peers because the pending-forward cap is full"
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
            if pending.peers.contains(&peer)
                || pending.peers.len() >= DHT_PENDING_FORWARD_PEERS_PER_TORRENT_CAP
                || self.pending_peer_count >= DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
            {
                break;
            }
            pending.peers.push(peer);
            self.pending_peer_count = self.pending_peer_count.saturating_add(1);
        }
    }

    fn flush_pending_peer_forwards(&mut self) {
        let hashes = self
            .pending_peer_forwards
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for info_hash in hashes {
            self.flush_pending_peer_forward(info_hash);
        }
    }

    fn flush_pending_peer_forward(&mut self, info_hash: [u8; 20]) {
        let Some(pending) = self.pending_peer_forwards.remove(&info_hash) else {
            return;
        };
        self.pending_peer_count = self.pending_peer_count.saturating_sub(pending.peers.len());
        match pending.cmd_tx.try_send(TorrentCmd::NewPeers(pending.peers)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(TorrentCmd::NewPeers(peers))) => {
                self.pending_peer_count = self.pending_peer_count.saturating_add(peers.len());
                self.pending_peer_forwards.insert(
                    info_hash,
                    PendingPeerForward {
                        cmd_tx: pending.cmd_tx,
                        peers,
                    },
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => self.remove_torrent(info_hash),
            Err(mpsc::error::TrySendError::Full(_)) => unreachable!(),
        }
    }

    fn remember_query_sender(&mut self, query: &DhtQuery, addr: SocketAddr) {
        let id = match query {
            DhtQuery::Ping { id }
            | DhtQuery::FindNode { id, .. }
            | DhtQuery::FindNodeWithWant { id, .. }
            | DhtQuery::GetPeers { id, .. }
            | DhtQuery::GetPeersWithWant { id, .. }
            | DhtQuery::AnnouncePeer { id, .. } => *id,
        };
        self.remember_node(id, addr);
    }

    fn remember_node(&mut self, id: NodeId, addr: SocketAddr) {
        if let SocketAddr::V6(addr) = addr {
            self.table.insert(KNode6 { id, addr });
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
            return false;
        }
        self.outstanding.insert(tx, query);
        true
    }

    fn prune_stale_outstanding(&mut self) -> Vec<[u8; 20]> {
        let mut hashes = HashSet::new();
        self.outstanding.retain(|_, query| {
            let fresh = query.sent_at.elapsed() < OUTSTANDING_QUERY_TTL;
            if !fresh {
                if let DhtRequest::GetPeers(info_hash) = query.request {
                    hashes.insert(info_hash);
                }
            }
            fresh
        });
        hashes.into_iter().collect()
    }

    fn update_stats(&self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.routing_nodes = self.table.total_nodes() as u64;
            stats.announced_peer_sets = self.announced_peers.len() as u64;
            stats.announced_peers = self.announced_peer_count as u64;
            stats.tracked_torrents_rejected = self.tracked_torrents_rejected;
            stats.outstanding_requests = self.outstanding.len() as u64;
            stats.queried_nodes = self.queried_node_count as u64;
        }
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
    total: &mut usize,
    info_hash: [u8; 20],
    peer: SocketAddr,
) -> bool {
    if let Some(peers) = peers_by_info_hash.get_mut(&info_hash) {
        // A duplicate is already represented and must remain an accepted
        // idempotent announce even when the global cache is full. New peers
        // must satisfy both the per-swarm and process-wide limits. The old
        // existing-swarm branch only checked the former, which let a single
        // known info-hash bypass DHT_ANNOUNCED_PEERS_GLOBAL_CAP entirely.
        if peers.contains(&peer) {
            return true;
        }
        if *total >= DHT_ANNOUNCED_PEERS_GLOBAL_CAP {
            return false;
        }
        let inserted = remember_announced_peer(peers, peer, DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP);
        if inserted {
            *total = total.saturating_add(1);
        }
        return inserted;
    }
    if peers_by_info_hash.len() >= DHT_ANNOUNCED_PEER_SET_CAP {
        return false;
    }
    if *total >= DHT_ANNOUNCED_PEERS_GLOBAL_CAP {
        return false;
    }
    peers_by_info_hash.insert(info_hash, vec![peer]);
    *total = total.saturating_add(1);
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

    fn test_egress_policy() -> OutboundEgressPolicy {
        OutboundEgressPolicy {
            allow_loopback: true,
            allow_private: true,
            allow_link_local: true,
            ..OutboundEgressPolicy::default()
        }
    }

    #[tokio::test]
    async fn run_dht_notifies_engine_after_binding() {
        let (dht_tx, mut dht_rx) = mpsc::channel(4);
        let (engine_tx, mut engine_rx) = mpsc::channel(4);
        let task = tokio::spawn(async move {
            run_dht(
                0,
                6881,
                Vec::new(),
                DhtRuntimeConfig {
                    tracked_torrents_cap: 16_384,
                    egress_policy: test_egress_policy(),
                },
                &mut dht_rx,
                VecDeque::new(),
                &engine_tx,
            )
            .await
        });

        assert!(matches!(
            timeout(Duration::from_secs(1), engine_rx.recv())
                .await
                .expect("DHT readiness notification timed out"),
            Some(EngineCmd::DhtTaskReady)
        ));

        let (reply, reply_rx) = oneshot::channel();
        dht_tx
            .send(DhtCommand::Shutdown { reply })
            .await
            .expect("DHT command channel should be open");
        timeout(Duration::from_secs(1), reply_rx)
            .await
            .expect("DHT shutdown acknowledgement timed out")
            .expect("DHT shutdown acknowledgement sender dropped");
        assert!(task.await.expect("DHT task panicked").is_ok());
    }

    #[tokio::test]
    async fn dht_loopback_addresses_require_explicit_egress_opt_in() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = peer_socket.local_addr().unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            tracked_torrents_cap: 16,
            egress_policy: OutboundEgressPolicy::default(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            generations: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
        };
        let info_hash = [9; 20];

        assert!(
            !task
                .send_get_peers(info_hash, peer_addr, DHT_QUERIED_NODES_GLOBAL_CAP)
                .await
        );
        assert!(task.outstanding.is_empty());

        let ping = KrpcMessage::Query {
            transaction_id: b"ping".to_vec(),
            query: DhtQuery::Ping {
                id: NodeId::from_bytes([2; 20]),
            },
        }
        .encode();
        task.handle_packet(&ping, peer_addr).await;
        assert_eq!(task.table.total_nodes(), 0);
        let mut packet = [0_u8; DHT_MAX_DATAGRAM_LEN];
        assert!(timeout(
            Duration::from_millis(20),
            peer_socket.recv_from(&mut packet)
        )
        .await
        .is_err());

        task.egress_policy.allow_loopback = true;
        assert!(
            task.send_get_peers(info_hash, peer_addr, DHT_QUERIED_NODES_GLOBAL_CAP)
                .await
        );
        let (length, _) = timeout(Duration::from_secs(1), peer_socket.recv_from(&mut packet))
            .await
            .expect("opted-in DHT query should reach the local peer")
            .unwrap();
        assert!(matches!(
            KrpcMessage::parse(&packet[..length]),
            Ok(KrpcMessage::Query { .. })
        ));

        task.handle_packet(&ping, peer_addr).await;
        assert_eq!(task.table.total_nodes(), 1);
        let (length, _) = timeout(Duration::from_secs(1), peer_socket.recv_from(&mut packet))
            .await
            .expect("opted-in DHT request should receive a response")
            .unwrap();
        assert!(matches!(
            KrpcMessage::parse(&packet[..length]),
            Ok(KrpcMessage::Response { .. })
        ));
    }

    #[tokio::test]
    async fn dht_ipv6_loopback_addresses_require_explicit_egress_opt_in() {
        let socket = match UdpSocket::bind("[::1]:0").await {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let peer_socket = match UdpSocket::bind("[::1]:0").await {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let SocketAddr::V6(peer_addr) = peer_socket.local_addr().unwrap() else {
            unreachable!("IPv6 loopback socket must have an IPv6 address")
        };
        let local_id = NodeId::from_bytes([1; 20]);
        let mut task = DhtV6Task {
            local_id,
            table: RoutingTable6::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            tracked_torrents_cap: 16,
            egress_policy: OutboundEgressPolicy::default(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            generations: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
            stats: Arc::new(Mutex::new(DhtV6Stats::default())),
        };
        let info_hash = [9; 20];
        let ping = KrpcMessage::Query {
            transaction_id: b"ping6".to_vec(),
            query: DhtQuery::Ping {
                id: NodeId::from_bytes([2; 20]),
            },
        }
        .encode();

        assert!(!task.send_get_peers(info_hash, peer_addr).await);
        assert!(task.outstanding.is_empty());
        task.handle_packet(&ping, SocketAddr::V6(peer_addr)).await;
        assert_eq!(task.table.total_nodes(), 0);
        let mut packet = [0_u8; DHT_MAX_DATAGRAM_LEN];
        assert!(timeout(
            Duration::from_millis(20),
            peer_socket.recv_from(&mut packet)
        )
        .await
        .is_err());

        task.egress_policy.allow_loopback = true;
        assert!(task.send_get_peers(info_hash, peer_addr).await);
        let (length, _) = timeout(Duration::from_secs(1), peer_socket.recv_from(&mut packet))
            .await
            .expect("opted-in IPv6 DHT query should reach the local peer")
            .unwrap();
        assert!(matches!(
            KrpcMessage::parse(&packet[..length]),
            Ok(KrpcMessage::Query { .. })
        ));

        task.handle_packet(&ping, SocketAddr::V6(peer_addr)).await;
        assert_eq!(task.table.total_nodes(), 1);
        let (length, _) = timeout(Duration::from_secs(1), peer_socket.recv_from(&mut packet))
            .await
            .expect("opted-in IPv6 DHT query should receive a response")
            .unwrap();
        assert!(matches!(
            KrpcMessage::parse(&packet[..length]),
            Ok(KrpcMessage::Response { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn forwarding_ipv6_command_has_bounded_queue_wait() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
        cmd_tx
            .try_send(DhtV6Command::Shutdown)
            .expect("test should fill the IPv6 command queue");
        let command = DhtCommand::RemoveTorrent {
            info_hash: [1; 20],
            generation: 1,
        };
        let mut forward = std::pin::pin!(forward_ipv6_command(Some(&cmd_tx), &command));
        tokio::task::yield_now().await;
        tokio::time::advance(DHT_IPV6_COMMAND_SEND_TIMEOUT).await;
        timeout(Duration::from_secs(1), &mut forward)
            .await
            .expect("full IPv6 command queue should not stall the IPv4 DHT actor");
        assert!(matches!(cmd_rx.try_recv(), Ok(DhtV6Command::Shutdown)));
    }

    #[tokio::test]
    async fn ipv6_generation_tombstones_are_bounded() {
        let socket = match bind_ipv6_socket(0) {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let local_id = NodeId::from_bytes([2; 20]);
        let tracked_torrents_cap = 2;
        let mut task = DhtV6Task {
            local_id,
            table: RoutingTable6::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            tracked_torrents_cap,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            generations: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
            stats: Arc::new(Mutex::new(DhtV6Stats::default())),
        };

        for index in 0..32_u64 {
            let mut info_hash = [0; 20];
            info_hash[..8].copy_from_slice(&index.to_be_bytes());
            task.handle_command(DhtV6Command::RemoveTorrent {
                info_hash,
                generation: 1,
            })
            .await;
        }

        assert!(task.generations.len() <= tracked_torrents_cap * 2);
    }

    #[tokio::test]
    async fn ipv6_pending_peer_forward_global_cap_does_not_create_empty_entries() {
        let socket = match bind_ipv6_socket(0) {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let local_id = NodeId::from_bytes([3; 20]);
        let first_hash = [1; 20];
        let second_hash = [2; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let mut task = DhtV6Task {
            local_id,
            table: RoutingTable6::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            generations: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::from([(
                first_hash,
                PendingPeerForward {
                    cmd_tx: cmd_tx.clone(),
                    peers: vec![SocketAddr::from(([2001, 0xdb8, 0, 0, 0, 0, 0, 1], 1))],
                },
            )]),
            pending_peer_count: DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP,
            last_bootstrap_at: None,
            stats: Arc::new(Mutex::new(DhtV6Stats::default())),
        };

        task.queue_pending_peer_forward(
            first_hash,
            cmd_tx.clone(),
            vec![SocketAddr::from(([2001, 0xdb8, 0, 0, 0, 0, 0, 2], 2))],
        );
        assert_eq!(task.pending_peer_forwards[&first_hash].peers.len(), 1);

        task.queue_pending_peer_forward(
            second_hash,
            cmd_tx,
            vec![SocketAddr::from(([2001, 0xdb8, 0, 0, 0, 0, 0, 3], 3))],
        );
        assert!(!task.pending_peer_forwards.contains_key(&second_hash));
        assert_eq!(task.pending_peer_forwards.len(), 1);
    }

    #[test]
    fn bootstrap_attempts_are_rate_limited() {
        let first = Instant::now();
        assert!(DhtTask::bootstrap_attempt_is_due(None, first));
        assert!(!DhtTask::bootstrap_attempt_is_due(Some(first), first));
        assert!(!DhtTask::bootstrap_attempt_is_due(
            Some(first),
            first + DHT_BOOTSTRAP_RETRY - Duration::from_nanos(1),
        ));
        assert!(DhtTask::bootstrap_attempt_is_due(
            Some(first),
            first + DHT_BOOTSTRAP_RETRY,
        ));
    }

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
            nodes6: Vec::new(),
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: u16::MAX,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
        let mut total = 0;
        for index in 0..DHT_ANNOUNCED_PEER_SET_CAP {
            let mut info_hash = [0_u8; 20];
            info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            assert!(remember_announced_peer_in_map(
                &mut peers,
                &mut total,
                info_hash,
                "198.51.100.1:6881".parse().unwrap(),
            ));
        }
        let mut next_hash = [0_u8; 20];
        next_hash[..8].copy_from_slice(&(DHT_ANNOUNCED_PEER_SET_CAP as u64).to_be_bytes());
        assert!(!remember_announced_peer_in_map(
            &mut peers,
            &mut total,
            next_hash,
            "198.51.100.2:6881".parse().unwrap(),
        ));
        assert_eq!(peers.len(), DHT_ANNOUNCED_PEER_SET_CAP);
        assert_eq!(total, DHT_ANNOUNCED_PEER_SET_CAP);
    }

    #[test]
    fn announced_peer_global_cap_applies_to_existing_info_hashes() {
        let mut peers = HashMap::new();
        let mut total = 0;
        for index in 0..(DHT_ANNOUNCED_PEERS_GLOBAL_CAP / DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP) {
            let mut info_hash = [0_u8; 20];
            info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            for port in 0..DHT_ANNOUNCED_PEERS_PER_INFO_HASH_CAP {
                assert!(remember_announced_peer_in_map(
                    &mut peers,
                    &mut total,
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
            &mut total,
            existing_hash,
            duplicate,
        ));
        assert!(!remember_announced_peer_in_map(
            &mut peers,
            &mut total,
            existing_hash,
            SocketAddr::from(([198, 51, 100, 2], 1)),
        ));
        assert_eq!(
            peers.values().map(Vec::len).sum::<usize>(),
            DHT_ANNOUNCED_PEERS_GLOBAL_CAP
        );
        assert_eq!(total, DHT_ANNOUNCED_PEERS_GLOBAL_CAP);
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
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
            pending_peer_count: DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP,
            last_bootstrap_at: None,
            generations: HashMap::new(),
        };

        assert_eq!(
            task.pending_peer_forwards
                .values()
                .map(|pending| pending.peers.len())
                .sum::<usize>(),
            DHT_PENDING_FORWARD_PEERS_GLOBAL_CAP
        );
        assert_eq!(
            task.pending_peer_count,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 1,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::from([(info_hash, vec![announced])]),
            announced_peer_count: 1,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 7,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 1,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([first]))]),
            queried_node_count: 1,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
        // A small cap here (rather than looping the production default
        // 131,072 times) both keeps this test fast and, combined with
        // `admission_cap_is_read_from_config_not_hardcoded` below, confirms
        // the admission check reads `tracked_torrents_cap` as a field value
        // rather than a hardcoded constant.
        const TEST_CAP: usize = 4;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let target = [255; 20];
        let (cmd_tx, _cmd_rx) = mpsc::channel(1);
        let mut tracked = HashMap::with_capacity(TEST_CAP);
        for index in 0..TEST_CAP {
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
            tracked_torrents_cap: TEST_CAP,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: tracked,
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
        // The rejection is observable via runtime stats, not just a log line.
        assert_eq!(task.runtime_stats().tracked_torrents_rejected, 1);
        assert_eq!(task.runtime_stats().tracked_torrents_cap, TEST_CAP as u64);

        task.torrents.remove(&[0; 20]);
        task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
            info_hash: target,
            cmd_tx,
            generation: 1,
        }))
        .await;

        assert!(task.torrents.contains_key(&target));
        // No further rejection once capacity was freed.
        assert_eq!(task.runtime_stats().tracked_torrents_rejected, 1);
    }

    /// The admission cap must come from `DhtTask::tracked_torrents_cap` (in
    /// turn sourced from `rt_config::DhtConfig::tracked_torrents_cap`), not
    /// from a hardcoded constant. Two tasks that differ only in that field
    /// must admit a different number of torrents.
    #[tokio::test]
    async fn admission_cap_is_read_from_config_not_hardcoded() {
        async fn build_task(cap: usize) -> DhtTask {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.set_nonblocking(true).unwrap();
            let socket = UdpSocket::from_std(socket).unwrap();
            let local_id = NodeId::from_bytes([1; 20]);
            DhtTask {
                local_id,
                table: RoutingTable::new(local_id),
                socket,
                listen_port: 6881,
                bootstrap_nodes: Vec::new(),
                tracked_torrents_cap: cap,
                egress_policy: test_egress_policy(),
                tracked_torrents_rejected: 0,
                next_tx: 1,
                outstanding: HashMap::new(),
                queried_nodes: HashMap::new(),
                queried_node_count: 0,
                torrents: HashMap::new(),
                announced_peers: HashMap::new(),
                announced_peer_count: 0,
                last_full_lookup: HashMap::new(),
                pending_peer_forwards: HashMap::new(),
                pending_peer_count: 0,
                last_bootstrap_at: None,
                generations: HashMap::new(),
            }
        }

        async fn admit(task: &mut DhtTask, count: usize) {
            let (cmd_tx, _cmd_rx) = mpsc::channel(1);
            for index in 0..count {
                let mut info_hash = [0; 20];
                info_hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
                task.handle_command(DhtCommand::AddTorrent(DhtTorrent {
                    info_hash,
                    cmd_tx: cmd_tx.clone(),
                    generation: (index as u64) + 1,
                }))
                .await;
            }
        }

        let mut small_cap = build_task(2).await;
        admit(&mut small_cap, 5).await;
        assert_eq!(small_cap.torrents.len(), 2);
        assert_eq!(small_cap.runtime_stats().tracked_torrents_rejected, 3);

        let mut larger_cap = build_task(5).await;
        admit(&mut larger_cap, 5).await;
        assert_eq!(larger_cap.torrents.len(), 5);
        assert_eq!(larger_cap.runtime_stats().tracked_torrents_rejected, 0);
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 0,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
            generations: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"gp".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                nodes6: Vec::new(),
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 0,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            generations: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
        };
        let response = KrpcMessage::Response {
            transaction_id: b"gp".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                nodes6: Vec::new(),
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 0,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
            generations: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"gp".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                nodes6: Vec::new(),
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
            generations: HashMap::new(),
        };
        let response = KrpcMessage::Response {
            transaction_id: b"zz".to_vec(),
            response: DhtResponse {
                id: remote_id,
                nodes: Vec::new(),
                nodes6: Vec::new(),
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 1,
            torrents: HashMap::from([(info_hash, cmd_tx.clone())]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::from([(info_hash, Instant::now())]),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
                nodes6: Vec::new(),
                values: vec![discovered_peer],
                token: None,
            },
        };
        task.handle_packet(&response.encode(), queried_addr).await;
        assert!(cmd_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn closed_torrent_registration_prunes_all_owned_state() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let socket = UdpSocket::from_std(socket).unwrap();
        let local_id = NodeId::from_bytes([1; 20]);
        let info_hash = [9; 20];
        let queried_addr: SocketAddrV4 = "127.0.0.1:6001".parse().unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        drop(cmd_rx);
        let mut task = DhtTask {
            local_id,
            table: RoutingTable::new(local_id),
            socket,
            listen_port: 6881,
            bootstrap_nodes: Vec::new(),
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::from([(
                b"lookup".to_vec(),
                OutstandingQuery {
                    addr: SocketAddr::V4(queried_addr),
                    request: DhtRequest::GetPeers(info_hash),
                    sent_at: Instant::now(),
                },
            )]),
            queried_nodes: HashMap::from([(info_hash, HashSet::from([queried_addr]))]),
            queried_node_count: 1,
            torrents: HashMap::from([(info_hash, cmd_tx.clone())]),
            announced_peers: HashMap::from([(info_hash, vec!["127.0.0.1:51413".parse().unwrap()])]),
            announced_peer_count: 1,
            last_full_lookup: HashMap::from([(info_hash, Instant::now())]),
            pending_peer_forwards: HashMap::from([(
                info_hash,
                PendingPeerForward {
                    cmd_tx,
                    peers: vec!["127.0.0.1:51414".parse().unwrap()],
                },
            )]),
            pending_peer_count: 1,
            last_bootstrap_at: None,
            generations: HashMap::from([(info_hash, 10)]),
        };

        task.prune_closed_torrents();

        assert!(!task.torrents.contains_key(&info_hash));
        assert!(task.announced_peers.is_empty());
        assert_eq!(task.announced_peer_count, 0);
        assert!(task.pending_peer_forwards.is_empty());
        assert_eq!(task.pending_peer_count, 0);
        assert!(task.outstanding.is_empty());
        assert!(task.queried_nodes.is_empty());
        assert_eq!(task.queried_node_count, 0);
        assert!(task.last_full_lookup.is_empty());
        assert_eq!(task.generations.get(&info_hash), Some(&10));
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
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
            queried_node_count: 0,
            torrents: HashMap::new(),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
            tracked_torrents_cap: 16_384,
            egress_policy: test_egress_policy(),
            tracked_torrents_rejected: 0,
            next_tx: 1,
            outstanding: HashMap::new(),
            queried_nodes: HashMap::new(),
            queried_node_count: 0,
            torrents: HashMap::from([(info_hash, cmd_tx)]),
            announced_peers: HashMap::new(),
            announced_peer_count: 0,
            last_full_lookup: HashMap::new(),
            pending_peer_forwards: HashMap::new(),
            pending_peer_count: 0,
            last_bootstrap_at: None,
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
        task.queried_node_count = initial.len();
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
