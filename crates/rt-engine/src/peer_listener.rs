//! Isolated TCP/uTP peer ingress boundary.
//!
//! The listener owns sockets and handshake work, while the engine actor owns
//! torrent promotion and command routing. Keeping the accept loop out of the
//! actor means an OS listener fault or a burst of slow handshakes cannot turn
//! the engine command loop into the socket supervisor.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use rt_peer_wire::handshake::{Handshake, HANDSHAKE_LEN};
use rt_utp::{UtpEndpoint, UtpError, UtpStream};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Notify, OwnedSemaphorePermit};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::command::EngineCmd;
use crate::engine::ENGINE_COMMAND_SEND_TIMEOUT;
use crate::network_budget::{GlobalNetworkBudget, PeerListenerRebindRequest};
use crate::peer_ingress::{PeerIngressBudget, PeerIngressPermit};
use crate::torrent_task::TorrentCmd;

pub(crate) struct ListenerCompletion {
    pub(crate) done: AtomicBool,
    pub(crate) notify: Notify,
}

impl ListenerCompletion {
    pub(crate) fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }
}

pub(crate) struct PeerListenerContext {
    pub(crate) peer_ingress: Arc<PeerIngressBudget>,
    pub(crate) network_budget: GlobalNetworkBudget,
    pub(crate) engine_tx: mpsc::Sender<EngineCmd>,
    pub(crate) healthy: Arc<AtomicBool>,
    pub(crate) done: Arc<ListenerCompletion>,
}

/// Run the socket acceptors independently of the engine command actor.
pub(crate) async fn run(
    mut listener: TcpListener,
    mut utp_endpoint: Option<UtpEndpoint>,
    context: PeerListenerContext,
    stop: watch::Receiver<bool>,
) {
    let _health_guard = ListenerHealthGuard {
        healthy: Arc::clone(&context.healthy),
        done: Arc::clone(&context.done),
    };
    let mut stop = stop;
    let mut rebind_rx = context.network_budget.take_listener_rebind_receiver();
    let mut handshakes = JoinSet::new();
    loop {
        let has_handshakes = !handshakes.is_empty();
        tokio::select! {
            stop_result = stop.changed() => {
                if stop_result.is_err() || *stop.borrow() {
                    break;
                }
            }
            request = receive_rebind_request(&mut rebind_rx) => {
                if let Some(request) = request {
                    let current_port = listener.local_addr().map(|addr| addr.port());
                    let result = match current_port {
                        Ok(port) if port == request.port => Ok(()),
                        Ok(_) => {
                            let incoming_utp = utp_endpoint.is_some();
                            match bind_peer_sockets(request.port, incoming_utp).await {
                                Ok((replacement_listener, replacement_utp)) => {
                                    listener = replacement_listener;
                                    if let Some(previous) =
                                        std::mem::replace(&mut utp_endpoint, replacement_utp)
                                    {
                                        if let Err(error) = previous.shutdown().await {
                                            warn!(
                                                component = "peer_listener",
                                                operation = "rebind_utp_shutdown",
                                                result = "error",
                                                error = %error,
                                                "old uTP listener failed to shut down after rebind"
                                            );
                                        }
                                    }
                                    info!(
                                        component = "peer_listener",
                                        operation = "rebind",
                                        port = request.port,
                                        "peer listener rebound"
                                    );
                                    Ok(())
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Err(error) => Err(format!("read peer listener address: {error}")),
                    };
                    let _ = request.reply.send(result);
                } else {
                    rebind_rx = None;
                }
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, peer_addr)) => {
                        context.healthy.store(true, Ordering::Release);
                        match context.peer_ingress.try_begin(peer_addr, Instant::now()) {
                            Ok(permit) => {
                                let Ok(peer_permit) = context.network_budget.try_acquire_peer() else {
                                    context
                                        .peer_ingress
                                        .record_peer_connection_budget_rejection();
                                    permit.cancel();
                                    warn!(
                                        component = "peer_listener",
                                        operation = "accept_peer",
                                        peer = %peer_addr,
                                        result = "rejected",
                                        reason = "global peer connection budget",
                                        "incoming peer rejected by global connection budget"
                                    );
                                    continue;
                                };
                                let engine_tx = context.engine_tx.clone();
                                let handshake_timeout = context.peer_ingress.config().handshake_timeout;
                                let peer_ingress = Arc::clone(&context.peer_ingress);
                                handshakes.spawn(async move {
                                    if let Err(error) = handle_incoming(
                                        stream,
                                        peer_addr,
                                        engine_tx,
                                        permit,
                                        peer_permit,
                                        &peer_ingress,
                                        handshake_timeout,
                                    )
                                    .await
                                    {
                                        warn!(
                                            component = "peer_listener",
                                            operation = "accept_peer",
                                            peer = %peer_addr,
                                            result = "error",
                                            error = %error,
                                            "incoming peer error"
                                        );
                                    }
                                });
                            }
                            Err(error) => {
                                warn!(
                                    component = "peer_listener",
                                    operation = "accept_peer",
                                    peer = %peer_addr,
                                    result = "rejected",
                                    reason = %error,
                                    "incoming peer rejected by handshake budget"
                                );
                            }
                        }
                    }
                    Err(error) => {
                                context.healthy.store(false, Ordering::Release);
                        warn!(
                            component = "peer_listener",
                            operation = "accept_peer",
                            result = "error",
                            error = %error,
                            "incoming peer listener is unhealthy; retrying"
                        );
                        if !backoff_or_stop(&mut stop).await {
                            break;
                        }
                    }
                }
            }
            utp_result = accept_utp_peer(utp_endpoint.as_ref()) => {
                match utp_result {
                    Ok((stream, peer_addr)) => {
                        match context.peer_ingress.try_begin(peer_addr, Instant::now()) {
                            Ok(permit) => {
                                let Ok(peer_permit) = context.network_budget.try_acquire_peer() else {
                                    context
                                        .peer_ingress
                                        .record_peer_connection_budget_rejection();
                                    permit.cancel();
                                    warn!(
                                        component = "peer_listener",
                                        operation = "accept_utp_peer",
                                        peer = %peer_addr,
                                        result = "rejected",
                                        reason = "global peer connection budget",
                                        "incoming uTP peer rejected by global connection budget"
                                    );
                                    continue;
                                };
                                let engine_tx = context.engine_tx.clone();
                                let handshake_timeout = context.peer_ingress.config().handshake_timeout;
                                let peer_ingress = Arc::clone(&context.peer_ingress);
                                handshakes.spawn(async move {
                                    if let Err(error) = handle_incoming_utp(
                                        stream,
                                        peer_addr,
                                        engine_tx,
                                        permit,
                                        peer_permit,
                                        &peer_ingress,
                                        handshake_timeout,
                                    )
                                    .await
                                    {
                                        warn!(
                                            component = "peer_listener",
                                            operation = "accept_utp_peer",
                                            peer = %peer_addr,
                                            result = "error",
                                            error = %error,
                                            "incoming uTP peer error"
                                        );
                                    }
                                });
                            }
                            Err(error) => {
                                warn!(
                                    component = "peer_listener",
                                    operation = "accept_utp_peer",
                                    peer = %peer_addr,
                                    result = "rejected",
                                    reason = %error,
                                    "incoming uTP peer rejected by handshake budget"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        warn!(
                            component = "peer_listener",
                            operation = "accept_utp_peer",
                            result = "error",
                            error = %error,
                            "uTP accept failed"
                        );
                        if utp_accept_failure_is_fatal(&error) {
                            // A failed endpoint receive loop is terminal for
                            // this listener incarnation. Continuing here
                            // would repeatedly observe `Closed` from the
                            // endpoint while the shared health flag stayed
                            // true because only the TCP branch updates it.
                            // Fail closed so readiness and metrics cannot
                            // claim that incoming peer work is available.
                            context.healthy.store(false, Ordering::Release);
                            break;
                        }
                        if !backoff_or_stop(&mut stop).await {
                            break;
                        }
                    }
                }
            }
            Some(result) = handshakes.join_next(), if has_handshakes => {
                if let Err(error) = result {
                    warn!(
                        component = "peer_listener",
                        operation = "handshake_task",
                        result = "error",
                        error = %error,
                        "incoming peer handshake task exited unexpectedly"
                    );
                }
            }
        }
    }

    if let Some(endpoint) = utp_endpoint.take() {
        if let Err(error) = endpoint.shutdown().await {
            warn!(
                component = "peer_listener",
                operation = "shutdown_utp_endpoint",
                result = "join_error",
                error = %error,
                "uTP receive task failed during shutdown"
            );
        }
    }

    // A stop signal must release every ingress and global-peer permit held by
    // a slow handshake immediately. Detached handshake tasks used to survive
    // the listener and could hold those limits until their read timeout.
    handshakes.abort_all();
    while let Some(result) = handshakes.join_next().await {
        if let Err(error) = result {
            debug_handshake_join_error(error);
        }
    }
}

async fn receive_rebind_request(
    receiver: &mut Option<mpsc::Receiver<PeerListenerRebindRequest>>,
) -> Option<PeerListenerRebindRequest> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn bind_peer_sockets(
    port: u16,
    incoming_utp: bool,
) -> Result<(TcpListener, Option<UtpEndpoint>), String> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| format!("binding TCP peer listener to {addr}: {error}"))?;
    let utp_endpoint = if incoming_utp {
        Some(
            UtpEndpoint::bind(addr)
                .await
                .map_err(|error| format!("binding uTP peer listener to {addr}: {error}"))?,
        )
    } else {
        None
    };
    Ok((listener, utp_endpoint))
}

fn debug_handshake_join_error(error: tokio::task::JoinError) {
    if !error.is_cancelled() {
        warn!(
            component = "peer_listener",
            operation = "handshake_shutdown",
            result = "error",
            error = %crate::task_join_error_summary("peer handshake task", &error),
            "incoming peer handshake task failed during listener shutdown"
        );
    }
}

async fn backoff_or_stop(stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = sleep(Duration::from_millis(50)) => !*stop.borrow(),
        changed = stop.changed() => changed.is_ok() && !*stop.borrow(),
    }
}

struct ListenerHealthGuard {
    healthy: Arc<AtomicBool>,
    done: Arc<ListenerCompletion>,
}

impl Drop for ListenerHealthGuard {
    fn drop(&mut self) {
        self.healthy.store(false, Ordering::Release);
        self.done.done.store(true, Ordering::Release);
        self.done.notify.notify_waiters();
    }
}

async fn handle_incoming(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    peer_ingress: &PeerIngressBudget,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    let mut hs = [0u8; HANDSHAKE_LEN];
    match timeout(handshake_timeout, stream.read_exact(&mut hs)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            peer_ingress.record_handshake_read_error();
            return Err(error).context("incoming TCP peer handshake read failed");
        }
        Err(error) => {
            peer_ingress.record_handshake_timeout();
            return Err(error).context("incoming TCP peer handshake timed out");
        }
    }
    let handshake = match Handshake::parse(&hs) {
        Ok(handshake) => handshake,
        Err(error) => {
            peer_ingress.record_malformed_handshake();
            return Err(error.into());
        }
    };
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let command = TorrentCmd::AcceptPeer {
        stream,
        peer_addr,
        handshake,
        peer_permit,
    };
    route_incoming_command(&info_hash_hex, engine_tx, command).await
}

async fn accept_utp_peer(
    endpoint: Option<&UtpEndpoint>,
) -> Result<(UtpStream, SocketAddr), UtpError> {
    let Some(endpoint) = endpoint else {
        std::future::pending::<()>().await;
        unreachable!("pending future never resolves");
    };
    let stream = endpoint.accept().await?;
    let peer_addr = stream.peer_addr();
    Ok((stream, peer_addr))
}

fn utp_accept_failure_is_fatal(error: &UtpError) -> bool {
    matches!(error, UtpError::Closed | UtpError::Io(_))
}

async fn handle_incoming_utp(
    mut stream: UtpStream,
    peer_addr: SocketAddr,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    peer_ingress: &PeerIngressBudget,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    let mut hs = [0u8; HANDSHAKE_LEN];
    match timeout(handshake_timeout, stream.read_exact(&mut hs)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            peer_ingress.record_handshake_read_error();
            return Err(error).context("incoming uTP peer handshake read failed");
        }
        Err(error) => {
            peer_ingress.record_handshake_timeout();
            return Err(error).context("incoming uTP peer handshake timed out");
        }
    }
    let handshake = match Handshake::parse(&hs) {
        Ok(handshake) => handshake,
        Err(error) => {
            peer_ingress.record_malformed_handshake();
            return Err(error.into());
        }
    };
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let command = TorrentCmd::AcceptUtpPeer {
        stream: Box::new(stream),
        peer_addr,
        handshake,
        peer_permit,
    };
    route_incoming_command(&info_hash_hex, engine_tx, command).await
}

async fn route_incoming_command(
    info_hash_hex: &str,
    engine_tx: mpsc::Sender<EngineCmd>,
    command: TorrentCmd,
) -> anyhow::Result<()> {
    timeout(
        ENGINE_COMMAND_SEND_TIMEOUT,
        engine_tx.send(EngineCmd::IncomingPeer {
            info_hash: info_hash_hex.to_owned(),
            command,
        }),
    )
    .await
    .map_err(|_| anyhow::anyhow!("engine command queue timed out"))?
    .map_err(|_| anyhow::anyhow!("engine stopped while routing inbound peer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{io::AsyncWriteExt, net::TcpStream, sync::Barrier};

    async fn tcp_pair() -> (TcpStream, TcpStream, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, peer_addr) = listener.accept().await.unwrap();
        (client, server, peer_addr)
    }

    #[tokio::test]
    async fn listener_completion_wakes_all_shutdown_waiters() {
        let completion = Arc::new(ListenerCompletion::new());
        let ready = Arc::new(Barrier::new(3));
        let waiters = (0..2)
            .map(|_| {
                let completion = Arc::clone(&completion);
                let ready = Arc::clone(&ready);
                tokio::spawn(async move {
                    let mut notified = std::pin::pin!(completion.notify.notified());
                    notified.as_mut().enable();
                    ready.wait().await;
                    notified.await;
                    completion.done.load(Ordering::Acquire)
                })
            })
            .collect::<Vec<_>>();

        ready.wait().await;
        let guard = ListenerHealthGuard {
            healthy: Arc::new(AtomicBool::new(true)),
            done: Arc::clone(&completion),
        };
        drop(guard);

        for waiter in waiters {
            assert!(timeout(Duration::from_secs(1), waiter)
                .await
                .expect("listener shutdown waiter timed out")
                .expect("listener shutdown waiter panicked"));
        }
    }

    #[tokio::test]
    async fn tcp_handshake_timeout_increments_ingress_counter() {
        let (_client, stream, peer_addr) = tcp_pair().await;
        let ingress = PeerIngressBudget::new(Default::default());
        let permit = ingress.try_begin(peer_addr, Instant::now()).unwrap();
        let peer_permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        let (engine_tx, _) = mpsc::channel(1);

        let error = handle_incoming(
            stream,
            peer_addr,
            engine_tx,
            permit,
            peer_permit,
            &ingress,
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("timed out"));
        assert_eq!(ingress.stats().handshake_timeouts, 1);
    }

    #[tokio::test]
    async fn malformed_tcp_handshake_increments_ingress_counter() {
        let (mut client, stream, peer_addr) = tcp_pair().await;
        let ingress = PeerIngressBudget::new(Default::default());
        let permit = ingress.try_begin(peer_addr, Instant::now()).unwrap();
        let peer_permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        let (engine_tx, _) = mpsc::channel(1);
        client.write_all(&[0; HANDSHAKE_LEN]).await.unwrap();

        assert!(handle_incoming(
            stream,
            peer_addr,
            engine_tx,
            permit,
            peer_permit,
            &ingress,
            Duration::from_secs(1),
        )
        .await
        .is_err());
        assert_eq!(ingress.stats().malformed_handshakes, 1);
    }

    #[tokio::test]
    async fn truncated_tcp_handshake_increments_read_error_counter() {
        let (client, stream, peer_addr) = tcp_pair().await;
        drop(client);
        let ingress = PeerIngressBudget::new(Default::default());
        let permit = ingress.try_begin(peer_addr, Instant::now()).unwrap();
        let peer_permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        let (engine_tx, _) = mpsc::channel(1);

        assert!(handle_incoming(
            stream,
            peer_addr,
            engine_tx,
            permit,
            peer_permit,
            &ingress,
            Duration::from_secs(1),
        )
        .await
        .is_err());
        assert_eq!(ingress.stats().handshake_read_errors, 1);
    }

    #[test]
    fn fatal_utp_accept_failures_are_distinguished_from_peer_rejections() {
        assert!(utp_accept_failure_is_fatal(&UtpError::Closed));
        assert!(utp_accept_failure_is_fatal(&UtpError::Io(
            "socket".to_owned()
        )));
        assert!(!utp_accept_failure_is_fatal(&UtpError::Timeout));
        assert!(!utp_accept_failure_is_fatal(
            &UtpError::InvalidStatePacket {
                state: rt_utp::ConnectionState::SynReceived,
                packet_type: rt_utp::PacketType::Data,
            }
        ));
    }
}
