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
use rt_utp::{UtpEndpoint, UtpStream};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Notify, OwnedSemaphorePermit};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tracing::warn;

use crate::command::EngineCmd;
use crate::engine::ENGINE_COMMAND_SEND_TIMEOUT;
use crate::network_budget::GlobalNetworkBudget;
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

/// Run the socket acceptors independently of the engine command actor.
pub(crate) async fn run(
    listener: TcpListener,
    utp_endpoint: Option<UtpEndpoint>,
    peer_ingress: Arc<PeerIngressBudget>,
    network_budget: GlobalNetworkBudget,
    engine_tx: mpsc::Sender<EngineCmd>,
    stop: watch::Receiver<bool>,
    healthy: Arc<AtomicBool>,
    done: Arc<ListenerCompletion>,
) {
    let _health_guard = ListenerHealthGuard {
        healthy: Arc::clone(&healthy),
        done,
    };
    let mut stop = stop;
    let mut handshakes = JoinSet::new();
    loop {
        let has_handshakes = !handshakes.is_empty();
        tokio::select! {
            stop_result = stop.changed() => {
                if stop_result.is_err() || *stop.borrow() {
                    break;
                }
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, peer_addr)) => {
                        healthy.store(true, Ordering::Release);
                        match peer_ingress.try_begin(peer_addr, Instant::now()) {
                            Ok(permit) => {
                                let Ok(peer_permit) = network_budget.try_acquire_peer() else {
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
                                let engine_tx = engine_tx.clone();
                                let handshake_timeout = peer_ingress.config().handshake_timeout;
                                handshakes.spawn(async move {
                                    if let Err(error) = handle_incoming(
                                        stream,
                                        peer_addr,
                                        engine_tx,
                                        permit,
                                        peer_permit,
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
                        healthy.store(false, Ordering::Release);
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
                        match peer_ingress.try_begin(peer_addr, Instant::now()) {
                            Ok(permit) => {
                                let Ok(peer_permit) = network_budget.try_acquire_peer() else {
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
                                let engine_tx = engine_tx.clone();
                                let handshake_timeout = peer_ingress.config().handshake_timeout;
                                handshakes.spawn(async move {
                                    if let Err(error) = handle_incoming_utp(
                                        stream,
                                        peer_addr,
                                        engine_tx,
                                        permit,
                                        peer_permit,
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

fn debug_handshake_join_error(error: tokio::task::JoinError) {
    if !error.is_cancelled() {
        warn!(
            component = "peer_listener",
            operation = "handshake_shutdown",
            result = "error",
            error = %error,
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
        self.done.notify.notify_one();
    }
}

async fn handle_incoming(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    let mut hs = [0u8; HANDSHAKE_LEN];
    timeout(handshake_timeout, stream.read_exact(&mut hs))
        .await
        .context("incoming TCP peer handshake timed out")??;
    let handshake = Handshake::parse(&hs)?;
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
) -> anyhow::Result<(UtpStream, SocketAddr)> {
    let Some(endpoint) = endpoint else {
        std::future::pending::<()>().await;
        unreachable!("pending future never resolves");
    };
    let stream = endpoint.accept().await?;
    let peer_addr = stream.peer_addr();
    Ok((stream, peer_addr))
}

async fn handle_incoming_utp(
    mut stream: UtpStream,
    peer_addr: SocketAddr,
    engine_tx: mpsc::Sender<EngineCmd>,
    _permit: PeerIngressPermit,
    peer_permit: OwnedSemaphorePermit,
    handshake_timeout: Duration,
) -> anyhow::Result<()> {
    let mut hs = [0u8; HANDSHAKE_LEN];
    timeout(handshake_timeout, stream.read_exact(&mut hs))
        .await
        .context("incoming uTP peer handshake timed out")??;
    let handshake = Handshake::parse(&hs)?;
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let command = TorrentCmd::AcceptUtpPeer {
        stream,
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
