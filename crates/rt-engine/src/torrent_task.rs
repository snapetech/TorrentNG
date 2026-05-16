/// Per-torrent async task.
///
/// One tokio task per torrent owns: tracker announce loop, peer connection
/// management, piece picker, and storage writes.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, RwLock};
use tokio::time::interval;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

use std::sync::Arc;

use rt_metainfo::TorrentMetaV1;
use rt_peer_wire::{
    codec::PeerCodec,
    handshake::{ExtensionFlags, Handshake},
    message::Message,
};
use rt_piece_picker::PiecePicker;
use rt_session::{SessionRegistry, TorrentState};

use crate::peer_id::OUR_PEER_ID;

/// Messages from the engine to a running torrent task.
#[derive(Debug)]
pub enum TorrentCmd {
    Pause,
    Resume,
    Recheck,
    Shutdown,
    /// Peers discovered by DHT or tracker.
    NewPeers(Vec<SocketAddr>),
}

/// A block received from a peer.
#[derive(Debug)]
pub struct BlockEvent {
    pub piece: u32,
    pub offset: u32,
    pub data: bytes::Bytes,
}

pub struct TorrentTask {
    info_hash_hex: String,
    meta: TorrentMetaV1,
    registry: Arc<RwLock<SessionRegistry>>,
    cmd_rx: mpsc::Receiver<TorrentCmd>,
    block_tx: mpsc::Sender<BlockEvent>,
    block_rx: mpsc::Receiver<BlockEvent>,
    picker: PiecePicker,
    /// active peer addresses
    active_peers: HashMap<SocketAddr, ()>,
    paused: bool,
    max_peers: usize,
}

impl TorrentTask {
    pub fn new(
        meta: TorrentMetaV1,
        registry: Arc<RwLock<SessionRegistry>>,
        cmd_rx: mpsc::Receiver<TorrentCmd>,
        max_peers: usize,
    ) -> Self {
        let (block_tx, block_rx) = mpsc::channel(256);
        let total = meta.total_length();
        let last_piece_len = if total % meta.piece_length == 0 {
            meta.piece_length
        } else {
            total % meta.piece_length
        };
        let piece_count = meta.pieces.len();
        let picker = PiecePicker::new(piece_count, meta.piece_length as u32, last_piece_len as u32);
        let info_hash_hex = meta.info_hash.iter().map(|b| format!("{b:02x}")).collect();
        TorrentTask {
            info_hash_hex,
            meta,
            registry,
            cmd_rx,
            block_tx,
            block_rx,
            picker,
            active_peers: HashMap::new(),
            paused: false,
            max_peers,
        }
    }

    pub async fn run(mut self) {
        let mut choke_tick = interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        TorrentCmd::Shutdown | TorrentCmd::Pause if matches!(cmd, TorrentCmd::Shutdown) => {
                            break;
                        }
                        TorrentCmd::Pause => {
                            self.paused = true;
                            self.active_peers.clear();
                            self.set_state(TorrentState::Paused).await;
                        }
                        TorrentCmd::Resume => {
                            self.paused = false;
                            self.set_state(TorrentState::Downloading).await;
                        }
                        TorrentCmd::NewPeers(addrs) => {
                            if !self.paused {
                                self.connect_peers(addrs).await;
                            }
                        }
                        TorrentCmd::Recheck => {
                            self.set_state(TorrentState::Checking).await;
                        }
                        TorrentCmd::Shutdown => unreachable!(),
                    }
                }

                Some(block) = self.block_rx.recv() => {
                    self.handle_block(block).await;
                }

                _ = choke_tick.tick() => {
                    // placeholder: full choker integration in Phase 2
                }
            }
        }
    }

    async fn connect_peers(&mut self, addrs: Vec<SocketAddr>) {
        for addr in addrs {
            if self.active_peers.len() >= self.max_peers {
                break;
            }
            if self.active_peers.contains_key(&addr) {
                continue;
            }
            self.active_peers.insert(addr, ());
            let info_hash = self.meta.info_hash;
            let block_tx = self.block_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = run_peer(addr, info_hash, block_tx).await {
                    debug!(peer = %addr, err = %e, "peer ended");
                }
            });
        }
    }

    async fn handle_block(&mut self, block: BlockEvent) {
        let complete = self
            .picker
            .block_received(block.piece as usize, block.offset);
        if complete {
            info!(piece = block.piece, torrent = %self.info_hash_hex, "piece complete");
            if self.picker.is_complete() {
                self.set_state(TorrentState::Seeding).await;
                info!(torrent = %self.info_hash_hex, "download complete");
            }
        }
    }

    async fn set_state(&self, state: TorrentState) {
        let mut reg = self.registry.write().await;
        if let Some(entry) = reg.get_mut(&self.info_hash_hex) {
            let _ = entry.transition(state);
        }
    }
}

/// Open a TCP connection, complete BEP 3 handshake, receive Piece messages.
async fn run_peer(
    addr: SocketAddr,
    info_hash: [u8; 20],
    block_tx: mpsc::Sender<BlockEvent>,
) -> anyhow::Result<()> {
    let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;
    stream.set_nodelay(true)?;

    let mut framed = Framed::new(stream, PeerCodec);

    let our_hs = Handshake {
        info_hash,
        peer_id: OUR_PEER_ID,
        reserved: ExtensionFlags::with_extension_protocol(),
    };
    // Send our handshake as raw bytes before the codec takes over.
    {
        use tokio::io::AsyncWriteExt;
        let inner = framed.get_mut();
        inner.write_all(&our_hs.encode()).await?;
    }

    // First message from remote is their 68-byte handshake; read it raw.
    {
        use tokio::io::AsyncReadExt;
        let mut hs_buf = [0u8; 68];
        framed.get_mut().read_exact(&mut hs_buf).await?;
        if hs_buf[28..48] != info_hash {
            anyhow::bail!("info_hash mismatch from {addr}");
        }
    }

    // Send Interested so the peer will unchoke us
    framed.send(Message::Interested).await?;

    while let Some(msg_result) = framed.next().await {
        match msg_result? {
            Message::Piece { piece, begin, data } => {
                if block_tx
                    .send(BlockEvent {
                        piece,
                        offset: begin,
                        data: bytes::Bytes::from(data),
                    })
                    .await
                    .is_err()
                {
                    break; // torrent task gone
                }
            }
            Message::Unchoke => {
                // We can now send requests; request all blocks for pieces the picker wants.
                // Full request loop is in the next integration step.
            }
            Message::Choke => {
                warn!(peer = %addr, "choked");
            }
            Message::KeepAlive => {}
            _ => {}
        }
    }
    Ok(())
}
