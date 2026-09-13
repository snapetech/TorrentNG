use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rand::RngExt;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch, Mutex},
    task::JoinHandle,
    time::timeout,
};

use crate::{
    error::UtpError,
    packet::{PacketType, UtpPacket},
    state::{InboundAction, UtpConnection, DEFAULT_MTU_PAYLOAD_BYTES},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtpTransportConfig {
    pub handshake_timeout: Duration,
    pub io_timeout: Duration,
    pub max_datagram_len: usize,
    pub max_retransmits: usize,
}

impl Default for UtpTransportConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(15),
            max_datagram_len: DEFAULT_MTU_PAYLOAD_BYTES + crate::HEADER_SIZE + 64,
            max_retransmits: 4,
        }
    }
}

pub struct UtpListener {
    socket: UdpSocket,
    config: UtpTransportConfig,
}

#[derive(Clone)]
pub struct UtpEndpoint {
    socket: Arc<UdpSocket>,
    config: UtpTransportConfig,
    accepted_rx: Arc<Mutex<mpsc::Receiver<Result<UtpStream, UtpError>>>>,
    stop: watch::Sender<bool>,
    recv_task: Arc<StdMutex<Option<JoinHandle<()>>>>,
}

pub struct UtpStream {
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    conn: UtpConnection,
    config: UtpTransportConfig,
    last_remote_timestamp_us: u32,
    read_buf: Vec<u8>,
    routed_rx: Option<mpsc::Receiver<UtpPacket>>,
    handshake_state: Option<UtpPacket>,
    route: Option<UtpRouteRegistration>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UtpStats {
    pub connects: u64,
    pub accepts: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub send_timeouts: u64,
    pub recv_timeouts: u64,
    pub retransmits: u64,
    pub route_drops: u64,
    pub rtt_samples: u64,
    pub rtt_us: u64,
    pub rtt_min_us: u64,
    pub rtt_max_us: u64,
    pub rtt_var_us: u64,
    pub retransmit_timeout_us: u64,
    pub congestion_window_bytes: u64,
    pub congestion_base_delay_us: u64,
    pub congestion_current_delay_us: u64,
    pub bytes_in_flight: u64,
}

static UTP_CONNECTS: AtomicU64 = AtomicU64::new(0);
static UTP_ACCEPTS: AtomicU64 = AtomicU64::new(0);
static UTP_BYTES_SENT: AtomicU64 = AtomicU64::new(0);
static UTP_BYTES_RECEIVED: AtomicU64 = AtomicU64::new(0);
static UTP_SEND_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static UTP_RECV_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static UTP_RETRANSMITS: AtomicU64 = AtomicU64::new(0);
static UTP_ROUTE_DROPS: AtomicU64 = AtomicU64::new(0);
static UTP_RTT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static UTP_RTT_US: AtomicU64 = AtomicU64::new(0);
static UTP_RTT_MIN_US: AtomicU64 = AtomicU64::new(0);
static UTP_RTT_MAX_US: AtomicU64 = AtomicU64::new(0);
static UTP_RTT_VAR_US: AtomicU64 = AtomicU64::new(0);
static UTP_RETRANSMIT_TIMEOUT_US: AtomicU64 = AtomicU64::new(0);
static UTP_CONGESTION_WINDOW_BYTES: AtomicU64 = AtomicU64::new(0);
static UTP_CONGESTION_BASE_DELAY_US: AtomicU64 = AtomicU64::new(0);
static UTP_CONGESTION_CURRENT_DELAY_US: AtomicU64 = AtomicU64::new(0);
static UTP_BYTES_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);

pub fn stats_snapshot() -> UtpStats {
    UtpStats {
        connects: UTP_CONNECTS.load(Ordering::Relaxed),
        accepts: UTP_ACCEPTS.load(Ordering::Relaxed),
        bytes_sent: UTP_BYTES_SENT.load(Ordering::Relaxed),
        bytes_received: UTP_BYTES_RECEIVED.load(Ordering::Relaxed),
        send_timeouts: UTP_SEND_TIMEOUTS.load(Ordering::Relaxed),
        recv_timeouts: UTP_RECV_TIMEOUTS.load(Ordering::Relaxed),
        retransmits: UTP_RETRANSMITS.load(Ordering::Relaxed),
        route_drops: UTP_ROUTE_DROPS.load(Ordering::Relaxed),
        rtt_samples: UTP_RTT_SAMPLES.load(Ordering::Relaxed),
        rtt_us: UTP_RTT_US.load(Ordering::Relaxed),
        rtt_min_us: UTP_RTT_MIN_US.load(Ordering::Relaxed),
        rtt_max_us: UTP_RTT_MAX_US.load(Ordering::Relaxed),
        rtt_var_us: UTP_RTT_VAR_US.load(Ordering::Relaxed),
        retransmit_timeout_us: UTP_RETRANSMIT_TIMEOUT_US.load(Ordering::Relaxed),
        congestion_window_bytes: UTP_CONGESTION_WINDOW_BYTES.load(Ordering::Relaxed),
        congestion_base_delay_us: UTP_CONGESTION_BASE_DELAY_US.load(Ordering::Relaxed),
        congestion_current_delay_us: UTP_CONGESTION_CURRENT_DELAY_US.load(Ordering::Relaxed),
        bytes_in_flight: UTP_BYTES_IN_FLIGHT.load(Ordering::Relaxed),
    }
}

fn observe_connection(conn: &UtpConnection) {
    UTP_RETRANSMIT_TIMEOUT_US.store(conn.retransmit_timeout_us(), Ordering::Relaxed);
    UTP_CONGESTION_WINDOW_BYTES.store(conn.congestion_window_bytes() as u64, Ordering::Relaxed);
    UTP_BYTES_IN_FLIGHT.store(conn.bytes_in_flight() as u64, Ordering::Relaxed);
    if let Some(rtt) = conn.rtt_us() {
        let rtt = rtt as u64;
        UTP_RTT_SAMPLES.fetch_add(1, Ordering::Relaxed);
        UTP_RTT_US.store(rtt, Ordering::Relaxed);
        record_nonzero_min(&UTP_RTT_MIN_US, rtt);
        UTP_RTT_MAX_US.fetch_max(rtt, Ordering::Relaxed);
    }
    if let Some(rtt_var) = conn.rtt_var_us() {
        UTP_RTT_VAR_US.store(rtt_var as u64, Ordering::Relaxed);
    }
    if let Some(base_delay) = conn.congestion_base_delay_us() {
        UTP_CONGESTION_BASE_DELAY_US.store(base_delay as u64, Ordering::Relaxed);
    }
    if let Some(current_delay) = conn.congestion_current_delay_us() {
        UTP_CONGESTION_CURRENT_DELAY_US.store(current_delay as u64, Ordering::Relaxed);
    }
}

fn record_nonzero_min(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    loop {
        if current != 0 && current <= value {
            return;
        }
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(next) => current = next,
        }
    }
}

impl std::fmt::Debug for UtpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtpStream")
            .field("peer", &self.peer)
            .field("state", &self.conn.state())
            .field("ids", &self.conn.ids())
            .field("routed", &self.routed_rx.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UtpRouteKey {
    peer: SocketAddr,
    recv_connection_id: u16,
}

struct UtpRoute {
    tx: mpsc::Sender<UtpPacket>,
    token: Arc<()>,
    handshake_state: UtpPacket,
}

struct UtpRouteCleanup {
    key: UtpRouteKey,
    token: Arc<()>,
}

struct UtpRouteRegistration {
    key: UtpRouteKey,
    token: Arc<()>,
    cleanup_tx: mpsc::UnboundedSender<UtpRouteCleanup>,
}

impl Drop for UtpRouteRegistration {
    fn drop(&mut self) {
        let _ = self.cleanup_tx.send(UtpRouteCleanup {
            key: self.key,
            token: Arc::clone(&self.token),
        });
    }
}

impl UtpListener {
    pub async fn bind(addr: SocketAddr) -> Result<Self, UtpError> {
        Self::bind_with_config(addr, UtpTransportConfig::default()).await
    }

    pub async fn bind_with_config(
        addr: SocketAddr,
        config: UtpTransportConfig,
    ) -> Result<Self, UtpError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        Ok(Self { socket, config })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, UtpError> {
        self.socket
            .local_addr()
            .map_err(|err| UtpError::Io(err.to_string()))
    }

    pub async fn accept(self) -> Result<UtpStream, UtpError> {
        let mut buf = vec![0u8; self.config.max_datagram_len.saturating_add(1)];
        loop {
            let (len, peer) = timeout(
                self.config.handshake_timeout,
                self.socket.recv_from(&mut buf),
            )
            .await
            .map_err(|_| UtpError::Timeout)?
            .map_err(|err| UtpError::Io(err.to_string()))?;
            if len > self.config.max_datagram_len {
                continue;
            }
            let packet = match UtpPacket::parse(&buf[..len]) {
                Ok(packet) => packet,
                Err(_error) => {
                    // Invalid UDP input is a datagram-level drop. Returning
                    // the parse error from this one-shot listener would let
                    // an unauthenticated packet permanently abort the next
                    // valid incoming handshake.
                    UTP_ROUTE_DROPS.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            if packet.header.packet_type != PacketType::Syn {
                continue;
            }

            self.socket
                .connect(peer)
                .await
                .map_err(|err| UtpError::Io(err.to_string()))?;
            let mut conn = UtpConnection::accept(&packet.header, random_seq_nr())?;
            let state = conn.build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
            send_packet(&self.socket, &state).await?;
            observe_connection(&conn);
            UTP_ACCEPTS.fetch_add(1, Ordering::Relaxed);
            return Ok(UtpStream {
                socket: Arc::new(self.socket),
                peer,
                conn,
                config: self.config,
                last_remote_timestamp_us: packet.header.timestamp_us,
                read_buf: Vec::new(),
                routed_rx: None,
                handshake_state: Some(state),
                route: None,
            });
        }
    }
}

impl UtpEndpoint {
    pub async fn bind(addr: SocketAddr) -> Result<Self, UtpError> {
        Self::bind_with_config(addr, UtpTransportConfig::default()).await
    }

    pub async fn bind_with_config(
        addr: SocketAddr,
        config: UtpTransportConfig,
    ) -> Result<Self, UtpError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        let socket = Arc::new(socket);
        let streams = Arc::new(Mutex::new(HashMap::new()));
        let (accepted_tx, accepted_rx) = mpsc::channel(256);
        let (stop, stop_rx) = watch::channel(false);
        let (route_cleanup_tx, route_cleanup_rx) = mpsc::unbounded_channel();
        let recv_task = tokio::spawn(run_endpoint_recv(
            socket.clone(),
            config,
            streams.clone(),
            accepted_tx,
            stop_rx,
            route_cleanup_tx,
            route_cleanup_rx,
        ));
        Ok(Self {
            socket,
            config,
            accepted_rx: Arc::new(Mutex::new(accepted_rx)),
            stop,
            recv_task: Arc::new(StdMutex::new(Some(recv_task))),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, UtpError> {
        self.socket
            .local_addr()
            .map_err(|err| UtpError::Io(err.to_string()))
    }

    pub async fn accept(&self) -> Result<UtpStream, UtpError> {
        timeout(self.config.handshake_timeout, async {
            self.accepted_rx
                .lock()
                .await
                .recv()
                .await
                .ok_or(UtpError::Closed)?
        })
        .await
        .map_err(|_| UtpError::Timeout)?
    }

    /// Stop the endpoint receive loop and wait for it to release its socket.
    ///
    /// The endpoint owns a shared UDP socket, so merely dropping the task
    /// handle is insufficient: the receive task would otherwise keep the
    /// socket bound until the runtime happens to tear it down.
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        let recv_task = self.recv_task.lock().ok().and_then(|mut task| task.take());
        if let Some(recv_task) = recv_task {
            let _ = recv_task.await;
        }
    }
}

impl Drop for UtpEndpoint {
    fn drop(&mut self) {
        // Explicit shutdown is used by the engine listener. Keep a best
        // effort abort fallback for callers that drop the last endpoint
        // handle without awaiting shutdown.
        if Arc::strong_count(&self.recv_task) != 1 {
            return;
        }
        let _ = self.stop.send(true);
        if let Ok(mut task) = self.recv_task.lock() {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
    }
}

async fn run_endpoint_recv(
    socket: Arc<UdpSocket>,
    config: UtpTransportConfig,
    streams: Arc<Mutex<HashMap<UtpRouteKey, UtpRoute>>>,
    accepted_tx: mpsc::Sender<Result<UtpStream, UtpError>>,
    mut stop: watch::Receiver<bool>,
    route_cleanup_tx: mpsc::UnboundedSender<UtpRouteCleanup>,
    mut route_cleanup_rx: mpsc::UnboundedReceiver<UtpRouteCleanup>,
) {
    let mut buf = vec![0u8; config.max_datagram_len.saturating_add(1)];
    loop {
        let recv_result = tokio::select! {
            stop_result = stop.changed() => {
                if stop_result.is_err() || *stop.borrow() {
                    break;
                }
                continue;
            }
            cleanup = route_cleanup_rx.recv() => {
                let Some(cleanup) = cleanup else {
                    break;
                };
                let mut streams = streams.lock().await;
                let remove = streams
                    .get(&cleanup.key)
                    .map(|route| Arc::ptr_eq(&route.token, &cleanup.token))
                    .unwrap_or(false);
                if remove {
                    streams.remove(&cleanup.key);
                }
                continue;
            }
            result = socket.recv_from(&mut buf) => result,
        };
        let (len, peer) = match recv_result {
            Ok(result) => result,
            Err(err) => {
                let _ = send_accepted(&accepted_tx, Err(UtpError::Io(err.to_string())), &mut stop)
                    .await;
                break;
            }
        };
        if len > config.max_datagram_len {
            UTP_ROUTE_DROPS.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let packet = match UtpPacket::parse(&buf[..len]) {
            Ok(packet) => packet,
            Err(_error) => {
                // Invalid UDP input is a route-level drop, not an endpoint
                // failure. Sending it through the bounded accept queue lets
                // an unauthenticated peer consume the queue and block valid
                // SYNs behind its parse errors.
                UTP_ROUTE_DROPS.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let key = UtpRouteKey {
            peer,
            recv_connection_id: packet.header.connection_id,
        };
        if packet.header.packet_type != PacketType::Syn {
            let tx = {
                let streams = streams.lock().await;
                streams.get(&key).map(|route| route.tx.clone())
            };
            if let Some(tx) = tx {
                if tx.try_send(packet).is_err() {
                    UTP_ROUTE_DROPS.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                UTP_ROUTE_DROPS.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        }

        // A SYN can be retransmitted when the handshake STATE was lost. If
        // the route already exists, resend the original STATE instead of
        // replacing the live stream and orphaning its receive queue.
        if let Some(state) = {
            let streams = streams.lock().await;
            streams.get(&key).map(|route| route.handshake_state.clone())
        } {
            if send_packet_to(&socket, peer, &state).await.is_err() {
                break;
            }
            continue;
        }

        let mut conn = match UtpConnection::accept(&packet.header, random_seq_nr()) {
            Ok(conn) => conn,
            Err(error) => {
                if !send_accepted(&accepted_tx, Err(error), &mut stop).await {
                    break;
                }
                continue;
            }
        };
        let state = conn.build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
        if let Err(error) = send_packet_to(&socket, peer, &state).await {
            if !send_accepted(&accepted_tx, Err(error), &mut stop).await {
                break;
            }
            continue;
        }

        let (tx, rx) = mpsc::channel(256);
        let key = UtpRouteKey {
            peer,
            recv_connection_id: conn.ids().recv,
        };
        let token = Arc::new(());
        streams.lock().await.insert(
            key,
            UtpRoute {
                tx,
                token: Arc::clone(&token),
                handshake_state: state.clone(),
            },
        );
        let stream = UtpStream {
            socket: socket.clone(),
            peer,
            conn,
            config,
            last_remote_timestamp_us: packet.header.timestamp_us,
            read_buf: Vec::new(),
            routed_rx: Some(rx),
            handshake_state: Some(state),
            route: Some(UtpRouteRegistration {
                key,
                token,
                cleanup_tx: route_cleanup_tx.clone(),
            }),
        };
        observe_connection(&stream.conn);
        UTP_ACCEPTS.fetch_add(1, Ordering::Relaxed);
        if !send_accepted(&accepted_tx, Ok(stream), &mut stop).await {
            break;
        }
    }
}

async fn send_accepted(
    accepted_tx: &mpsc::Sender<Result<UtpStream, UtpError>>,
    stream: Result<UtpStream, UtpError>,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        result = accepted_tx.send(stream) => result.is_ok(),
        changed = stop.changed() => changed.is_ok() && !*stop.borrow(),
    }
}

impl UtpStream {
    pub async fn connect(peer: SocketAddr) -> Result<Self, UtpError> {
        Self::connect_with_config(peer, UtpTransportConfig::default()).await
    }

    pub async fn connect_with_config(
        peer: SocketAddr,
        config: UtpTransportConfig,
    ) -> Result<Self, UtpError> {
        let bind_addr = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind_addr)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        socket
            .connect(peer)
            .await
            .map_err(|err| UtpError::Io(err.to_string()))?;
        let mut conn = UtpConnection::connect(random_connection_id(), random_seq_nr());
        let syn = conn.build_syn(now_us());
        let mut packet = None;
        for _ in 0..=config.max_retransmits {
            send_packet(&socket, &syn).await?;
            match recv_packet(&socket, config.handshake_timeout, config.max_datagram_len).await {
                Ok(received) => {
                    packet = Some(received);
                    break;
                }
                Err(UtpError::Timeout) => continue,
                Err(err) => return Err(err),
            }
        }
        let packet = packet.ok_or(UtpError::Timeout)?;
        conn.on_inbound(&packet)?;
        observe_connection(&conn);
        if !conn.is_established() {
            return Err(UtpError::InvalidStatePacket {
                state: conn.state(),
                packet_type: packet.header.packet_type,
            });
        }
        UTP_CONNECTS.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            socket: Arc::new(socket),
            peer,
            conn,
            config,
            last_remote_timestamp_us: packet.header.timestamp_us,
            read_buf: Vec::new(),
            routed_rx: None,
            handshake_state: None,
            route: None,
        })
    }

    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    pub fn connection(&self) -> &UtpConnection {
        &self.conn
    }

    pub async fn send(&mut self, payload: &[u8]) -> Result<(), UtpError> {
        for chunk in payload.chunks(DEFAULT_MTU_PAYLOAD_BYTES) {
            let packet = self.conn.build_data(
                now_us(),
                timestamp_diff_us(self.last_remote_timestamp_us),
                chunk.to_vec(),
            );
            let mut acknowledged = false;
            for attempt in 0..=self.config.max_retransmits {
                if attempt > 0 {
                    UTP_RETRANSMITS.fetch_add(1, Ordering::Relaxed);
                }
                self.send_packet(&packet).await?;
                loop {
                    let ack = match self.recv_packet(self.config.io_timeout).await {
                        Ok(ack) => ack,
                        Err(UtpError::Timeout) => {
                            self.conn.on_timeout();
                            observe_connection(&self.conn);
                            UTP_SEND_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                            break;
                        }
                        Err(err) => return Err(err),
                    };
                    self.last_remote_timestamp_us = ack.header.timestamp_us;
                    let action = self.conn.on_inbound(&ack)?;
                    match action {
                        InboundAction::DeliverPayload => {
                            let response = self
                                .conn
                                .build_state(now_us(), timestamp_diff_us(ack.header.timestamp_us));
                            self.send_packet(&response).await?;
                            self.read_buf.extend_from_slice(&ack.payload);
                            UTP_BYTES_RECEIVED
                                .fetch_add(ack.payload.len() as u64, Ordering::Relaxed);
                        }
                        InboundAction::SendState => {
                            let response = self
                                .conn
                                .build_state(now_us(), timestamp_diff_us(ack.header.timestamp_us));
                            self.send_packet(&response).await?;
                        }
                        InboundAction::Close => {
                            let response = self
                                .conn
                                .build_state(now_us(), timestamp_diff_us(ack.header.timestamp_us));
                            self.send_packet(&response).await?;
                            self.conn.mark_closed();
                            return Err(UtpError::Closed);
                        }
                        InboundAction::Reset => {
                            self.conn.mark_closed();
                            return Err(UtpError::Closed);
                        }
                        InboundAction::None => {}
                    }
                    observe_connection(&self.conn);
                    if self.conn.all_sent_packets_acked() {
                        acknowledged = true;
                        break;
                    }
                }
                if acknowledged {
                    break;
                }
            }
            if !acknowledged {
                UTP_SEND_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                return Err(UtpError::Timeout);
            }
        }
        Ok(())
    }

    pub async fn write_all(&mut self, mut bytes: &[u8]) -> Result<(), UtpError> {
        while !bytes.is_empty() {
            let len = bytes.len().min(DEFAULT_MTU_PAYLOAD_BYTES);
            self.send(&bytes[..len]).await?;
            bytes = &bytes[len..];
        }
        Ok(())
    }

    pub async fn read_exact(&mut self, out: &mut [u8]) -> Result<(), UtpError> {
        let mut filled = 0;
        while filled < out.len() {
            if self.read_buf.is_empty() {
                self.read_buf = self.recv().await?;
                if self.read_buf.is_empty() {
                    return Err(UtpError::Closed);
                }
            }
            let n = (out.len() - filled).min(self.read_buf.len());
            out[filled..filled + n].copy_from_slice(&self.read_buf[..n]);
            self.read_buf.drain(..n);
            filled += n;
        }
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Vec<u8>, UtpError> {
        if !self.read_buf.is_empty() {
            return Ok(std::mem::take(&mut self.read_buf));
        }
        loop {
            let packet = self.recv_packet(self.config.io_timeout).await?;
            self.last_remote_timestamp_us = packet.header.timestamp_us;
            if packet.header.packet_type == PacketType::Syn
                && packet.header.connection_id == self.conn.ids().recv
            {
                if let Some(state) = self.handshake_state.as_ref() {
                    self.send_packet(state).await?;
                    continue;
                }
            }
            let action = self.conn.on_inbound(&packet)?;
            observe_connection(&self.conn);
            match action {
                InboundAction::DeliverPayload => {
                    let ack = self
                        .conn
                        .build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
                    self.send_packet(&ack).await?;
                    UTP_BYTES_RECEIVED.fetch_add(packet.payload.len() as u64, Ordering::Relaxed);
                    return Ok(packet.payload);
                }
                InboundAction::Close => {
                    let ack = self
                        .conn
                        .build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
                    self.send_packet(&ack).await?;
                    self.conn.mark_closed();
                    return Ok(Vec::new());
                }
                InboundAction::Reset => {
                    self.conn.mark_closed();
                    return Ok(Vec::new());
                }
                InboundAction::SendState => {
                    let ack = self
                        .conn
                        .build_state(now_us(), timestamp_diff_us(packet.header.timestamp_us));
                    self.send_packet(&ack).await?;
                }
                InboundAction::None => {}
            }
        }
    }

    pub async fn close(&mut self) -> Result<(), UtpError> {
        let fin = self
            .conn
            .build_fin(now_us(), timestamp_diff_us(self.last_remote_timestamp_us));
        let mut acknowledged = false;
        for attempt in 0..=self.config.max_retransmits {
            if attempt > 0 {
                UTP_RETRANSMITS.fetch_add(1, Ordering::Relaxed);
            }
            self.send_packet(&fin).await?;
            loop {
                let received = match self.recv_packet(self.config.io_timeout).await {
                    Ok(received) => received,
                    Err(UtpError::Timeout) => {
                        self.conn.on_timeout();
                        observe_connection(&self.conn);
                        UTP_SEND_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    Err(err) => return Err(err),
                };
                self.last_remote_timestamp_us = received.header.timestamp_us;
                let action = self.conn.on_inbound(&received)?;
                match action {
                    InboundAction::DeliverPayload => {
                        let ack = self
                            .conn
                            .build_state(now_us(), timestamp_diff_us(received.header.timestamp_us));
                        self.send_packet(&ack).await?;
                        self.read_buf.extend_from_slice(&received.payload);
                        UTP_BYTES_RECEIVED
                            .fetch_add(received.payload.len() as u64, Ordering::Relaxed);
                    }
                    InboundAction::SendState => {
                        let ack = self
                            .conn
                            .build_state(now_us(), timestamp_diff_us(received.header.timestamp_us));
                        self.send_packet(&ack).await?;
                    }
                    InboundAction::Close => {
                        let ack = self
                            .conn
                            .build_state(now_us(), timestamp_diff_us(received.header.timestamp_us));
                        self.send_packet(&ack).await?;
                        self.conn.mark_closed();
                        return Ok(());
                    }
                    InboundAction::Reset => {
                        self.conn.mark_closed();
                        return Ok(());
                    }
                    InboundAction::None => {}
                }
                observe_connection(&self.conn);
                if self.conn.all_sent_packets_acked() {
                    acknowledged = true;
                    break;
                }
            }
            if acknowledged {
                break;
            }
        }
        if !acknowledged {
            return Err(UtpError::Timeout);
        }
        self.conn.mark_closed();
        Ok(())
    }

    async fn send_packet(&self, packet: &UtpPacket) -> Result<(), UtpError> {
        if self.routed_rx.is_some() {
            send_packet_to(&self.socket, self.peer, packet).await
        } else {
            send_packet(&self.socket, packet).await
        }
    }

    async fn recv_packet(&mut self, wait: Duration) -> Result<UtpPacket, UtpError> {
        if let Some(rx) = &mut self.routed_rx {
            timeout(wait, rx.recv())
                .await
                .map_err(|_| UtpError::Timeout)?
                .ok_or(UtpError::Closed)
        } else {
            recv_packet(&self.socket, wait, self.config.max_datagram_len).await
        }
    }
}

impl Drop for UtpStream {
    fn drop(&mut self) {
        // Endpoint routes are removed asynchronously by the receive loop.
        // The token prevents a late cleanup from deleting a newer stream if
        // the same peer/connection-id pair is ever reused.
        drop(self.route.take());
    }
}

async fn send_packet(socket: &UdpSocket, packet: &UtpPacket) -> Result<(), UtpError> {
    let bytes = packet.encode()?;
    socket
        .send(&bytes)
        .await
        .map(|written| {
            UTP_BYTES_SENT.fetch_add(written as u64, Ordering::Relaxed);
        })
        .map_err(|err| UtpError::Io(err.to_string()))
}

async fn send_packet_to(
    socket: &UdpSocket,
    peer: SocketAddr,
    packet: &UtpPacket,
) -> Result<(), UtpError> {
    let bytes = packet.encode()?;
    socket
        .send_to(&bytes, peer)
        .await
        .map(|written| {
            UTP_BYTES_SENT.fetch_add(written as u64, Ordering::Relaxed);
        })
        .map_err(|err| UtpError::Io(err.to_string()))
}

async fn recv_packet(
    socket: &UdpSocket,
    wait: Duration,
    max_datagram_len: usize,
) -> Result<UtpPacket, UtpError> {
    let mut buf = vec![0u8; max_datagram_len.saturating_add(1)];
    let len = timeout(wait, socket.recv(&mut buf))
        .await
        .map_err(|_| {
            UTP_RECV_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            UtpError::Timeout
        })?
        .map_err(|err| UtpError::Io(err.to_string()))?;
    if len > max_datagram_len {
        return Err(UtpError::DatagramTooLarge {
            actual: len,
            max: max_datagram_len,
        });
    }
    UtpPacket::parse(&buf[..len])
}

fn random_connection_id() -> u16 {
    rand::rng().random()
}

fn random_seq_nr() -> u16 {
    rand::rng().random()
}

fn now_us() -> u32 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_micros() as u32
}

fn timestamp_diff_us(remote_timestamp_us: u32) -> u32 {
    now_us().wrapping_sub(remote_timestamp_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> UtpTransportConfig {
        UtpTransportConfig {
            handshake_timeout: Duration::from_secs(2),
            io_timeout: Duration::from_secs(2),
            max_datagram_len: 2048,
            max_retransmits: 1,
        }
    }

    #[tokio::test]
    async fn utp_stream_connects_and_exchanges_payload() {
        let before = stats_snapshot();
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let payload = stream.recv().await.unwrap();
            assert_eq!(payload, b"hello over utp");
            stream.send(b"ack").await.unwrap();
            stream.close().await.unwrap();
        });

        let mut client = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        client.send(b"hello over utp").await.unwrap();
        assert_eq!(client.recv().await.unwrap(), b"ack");
        let _ = client.recv().await.unwrap();
        server.await.unwrap();
        let after = stats_snapshot();
        assert!(after.connects > before.connects);
        assert!(after.accepts > before.accepts);
        assert!(after.bytes_sent > before.bytes_sent);
        assert!(after.bytes_received > before.bytes_received);
    }

    #[tokio::test]
    async fn utp_stream_read_exact_spans_payload_chunks() {
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            stream.write_all(b"hello").await.unwrap();
            stream.write_all(b"world").await.unwrap();
        });

        let mut client = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        let mut first = [0u8; 3];
        let mut second = [0u8; 7];
        client.read_exact(&mut first).await.unwrap();
        client.read_exact(&mut second).await.unwrap();
        assert_eq!(&first, b"hel");
        assert_eq!(&second, b"loworld");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn utp_endpoint_accepts_multiple_streams_on_one_socket() {
        let endpoint = UtpEndpoint::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut first = endpoint.accept().await.unwrap();
            let mut second = endpoint.accept().await.unwrap();
            assert_eq!(first.recv().await.unwrap(), b"first");
            assert_eq!(second.recv().await.unwrap(), b"second");
            first.send(b"ack-first").await.unwrap();
            second.send(b"ack-second").await.unwrap();
        });

        let mut first = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        let mut second = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        first.send(b"first").await.unwrap();
        second.send(b"second").await.unwrap();
        assert_eq!(first.recv().await.unwrap(), b"ack-first");
        assert_eq!(second.recv().await.unwrap(), b"ack-second");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn utp_stream_preserves_simultaneous_payloads_while_acknowledging() {
        let config = UtpTransportConfig {
            handshake_timeout: Duration::from_secs(1),
            io_timeout: Duration::from_millis(100),
            max_datagram_len: 2048,
            max_retransmits: 1,
        };
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            stream.send(b"server-to-client").await.unwrap();
            stream
        });

        let mut client = UtpStream::connect_with_config(addr, config).await.unwrap();
        client.send(b"client-to-server").await.unwrap();
        assert_eq!(client.recv().await.unwrap(), b"server-to-client");

        let mut server = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("simultaneous uTP send timed out")
            .unwrap();
        assert_eq!(server.recv().await.unwrap(), b"client-to-server");
    }

    #[tokio::test]
    async fn utp_endpoint_shutdown_releases_socket() {
        let endpoint = UtpEndpoint::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = endpoint.local_addr().unwrap();
        endpoint.shutdown().await;
        drop(endpoint);

        let rebound = UtpEndpoint::bind_with_config(addr, test_config())
            .await
            .expect("endpoint shutdown must release the UDP socket");
        rebound.shutdown().await;
    }

    #[tokio::test]
    async fn utp_endpoint_shutdown_closes_pending_accept() {
        let endpoint = UtpEndpoint::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let waiter = {
            let endpoint = endpoint.clone();
            tokio::spawn(async move { endpoint.accept().await })
        };

        endpoint.shutdown().await;
        assert!(matches!(waiter.await.unwrap(), Err(UtpError::Closed)));
    }

    #[tokio::test]
    async fn malformed_endpoint_datagram_does_not_poison_accept_queue() {
        let endpoint = UtpEndpoint::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = endpoint.local_addr().unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .send_to(&[0u8; crate::HEADER_SIZE], addr)
            .await
            .unwrap();

        let accept_waiter = {
            let endpoint = endpoint.clone();
            tokio::spawn(async move { endpoint.accept().await })
        };
        let client = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        let accepted = timeout(Duration::from_secs(2), accept_waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        drop(client);
        drop(accepted);
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_listener_datagram_does_not_abort_next_handshake() {
        let listener = UtpListener::bind_with_config("127.0.0.1:0".parse().unwrap(), test_config())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .send_to(&[0u8; crate::HEADER_SIZE], addr)
            .await
            .unwrap();

        let accept_waiter = tokio::spawn(async move { listener.accept().await });
        let client = UtpStream::connect_with_config(addr, test_config())
            .await
            .unwrap();
        let accepted = timeout(Duration::from_secs(2), accept_waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        drop(client);
        drop(accepted);
    }

    #[tokio::test]
    async fn recv_packet_rejects_an_oversized_datagram_instead_of_parsing_a_truncation() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        receiver
            .connect(sender.local_addr().unwrap())
            .await
            .unwrap();

        let packet = UtpPacket {
            header: crate::packet::UtpHeader {
                packet_type: PacketType::Data,
                version: 1,
                extension: 0,
                connection_id: 1,
                timestamp_us: 0,
                timestamp_diff: 0,
                wnd_size: 1,
                seq_nr: 1,
                ack_nr: 0,
            },
            extensions: Vec::new(),
            payload: vec![42],
        };
        let bytes = packet.encode().unwrap();
        sender
            .send_to(&bytes, receiver.local_addr().unwrap())
            .await
            .unwrap();

        let error = recv_packet(&receiver, Duration::from_secs(1), crate::HEADER_SIZE)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            UtpError::DatagramTooLarge { actual, max }
                if actual == crate::HEADER_SIZE + 1 && max == crate::HEADER_SIZE
        ));
    }
}
