/// Top-level engine: manages torrent task lifecycle and incoming TCP listener.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use rt_config::Config;
use rt_metainfo::TorrentMeta;
use rt_session::{SessionRegistry, TorrentEntry, TorrentState};

use crate::command::{CmdResult, EngineCmd};
use crate::torrent_task::{TorrentCmd, TorrentTask};

/// Handle given to the API layer. Clone freely; all sends are channel-based.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineCmd>,
}

impl EngineHandle {
    /// Add a torrent; returns the v1 info_hash hex string.
    pub async fn add_torrent(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddTorrent {
                meta: Box::new(meta),
                save_path,
                paused,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn remove_torrent(&self, info_hash: String, delete_files: bool) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RemoveTorrent {
                info_hash,
                delete_files,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn pause_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::PauseTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn resume_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ResumeTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn shutdown(&self) {
        let _ = self.tx.send(EngineCmd::Shutdown).await;
    }
}

/// The running engine.
pub struct Engine {
    config: Arc<Config>,
    registry: Arc<RwLock<SessionRegistry>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    /// info_hash_hex → channel to the torrent task
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
}

impl Engine {
    /// Spawn the engine, returning an EngineHandle for the API layer.
    pub async fn start(
        config: Arc<Config>,
        registry: Arc<RwLock<SessionRegistry>>,
    ) -> anyhow::Result<EngineHandle> {
        let (tx, cmd_rx) = mpsc::channel(64);
        let handle = EngineHandle { tx };

        let engine = Engine {
            config: config.clone(),
            registry,
            cmd_rx,
            torrent_chans: HashMap::new(),
        };

        // Spawn TCP listener
        let listen_addr: SocketAddr = format!("0.0.0.0:{}", config.network.listen_port)
            .parse()
            .context("invalid listen_port")?;
        let listener = TcpListener::bind(listen_addr).await?;
        info!(addr = %listen_addr, "TCP peer listener bound");

        tokio::spawn(engine.run(listener));
        Ok(handle)
    }

    async fn run(mut self, listener: TcpListener) {
        loop {
            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    if !self.handle_cmd(cmd).await {
                        break;
                    }
                }
                Ok((stream, peer_addr)) = listener.accept() => {
                    // Incoming peer connection — hand off to a task that
                    // reads the handshake and routes to the right torrent task.
                    let chans = self.torrent_chans.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_incoming(stream, peer_addr, chans).await {
                            warn!(peer = %peer_addr, err = %e, "incoming peer error");
                        }
                    });
                }
            }
        }
        // Shutdown all torrent tasks
        for (_, tx) in self.torrent_chans.drain() {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
        info!("engine shut down");
    }

    /// Returns false if the engine should stop.
    async fn handle_cmd(&mut self, cmd: EngineCmd) -> bool {
        match cmd {
            EngineCmd::Shutdown => return false,

            EngineCmd::AddTorrent {
                meta,
                save_path,
                paused,
                reply,
            } => {
                let result = self.add_torrent(*meta, save_path, paused).await;
                let _ = reply.send(result);
            }

            EngineCmd::RemoveTorrent {
                info_hash,
                delete_files: _,
                reply,
            } => {
                let result = if let Some(tx) = self.torrent_chans.remove(&info_hash) {
                    let _ = tx.send(TorrentCmd::Shutdown).await;
                    let mut reg = self.registry.write().await;
                    reg.remove(&info_hash)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                } else {
                    Err(format!("torrent {info_hash} not found"))
                };
                let _ = reply.send(result);
            }

            EngineCmd::PauseTorrent { info_hash, reply } => {
                let result = self.send_to_torrent(&info_hash, TorrentCmd::Pause).await;
                let _ = reply.send(result);
            }

            EngineCmd::ResumeTorrent { info_hash, reply } => {
                let result = self.send_to_torrent(&info_hash, TorrentCmd::Resume).await;
                let _ = reply.send(result);
            }

            EngineCmd::RecheckTorrent { info_hash, reply } => {
                let result = self.send_to_torrent(&info_hash, TorrentCmd::Recheck).await;
                let _ = reply.send(result);
            }
        }
        true
    }

    async fn add_torrent(
        &mut self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
    ) -> CmdResult<String> {
        let v1 = match meta {
            TorrentMeta::V1(m) => m,
            TorrentMeta::Hybrid(m, _) => m,
            TorrentMeta::V2(_) => {
                return Err("pure v2 torrents not yet supported for seeding".to_owned());
            }
        };

        let info_hash_hex: String = v1.info_hash.iter().map(|b| format!("{b:02x}")).collect();

        if self.torrent_chans.contains_key(&info_hash_hex) {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());

        // Register in session
        {
            let mut reg = self.registry.write().await;
            let entry = TorrentEntry::new(
                info_hash_hex.clone(),
                v1.name.clone(),
                save.to_string_lossy().into_owned(),
            );
            reg.add(entry).map_err(|e| e.to_string())?;
            // TorrentEntry starts in Stopped; transition to target state.
            let target = if paused {
                TorrentState::Paused
            } else {
                TorrentState::Downloading
            };
            if let Some(e) = reg.get_mut(&info_hash_hex) {
                let _ = e.transition(target);
            }
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);

        let task = TorrentTask::new(
            v1,
            Arc::clone(&self.registry),
            cmd_rx,
            self.config.network.max_peers,
        );

        tokio::spawn(task.run());

        self.torrent_chans.insert(info_hash_hex.clone(), cmd_tx);
        info!(torrent = %info_hash_hex, paused, "torrent added");
        Ok(info_hash_hex)
    }

    async fn send_to_torrent(&self, info_hash: &str, cmd: TorrentCmd) -> CmdResult<()> {
        match self.torrent_chans.get(info_hash) {
            Some(tx) => tx
                .send(cmd)
                .await
                .map_err(|_| "torrent task gone".to_owned()),
            None => Err(format!("torrent {info_hash} not found")),
        }
    }
}

/// Handle an incoming TCP peer connection: read the handshake's info_hash and
/// forward the stream to the matching torrent task.
async fn handle_incoming(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    _torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut hs = [0u8; 68];
    stream.read_exact(&mut hs).await?;
    // protocol string check
    if hs[0] != 19 || &hs[1..20] != b"BitTorrent protocol" {
        anyhow::bail!("not a BitTorrent handshake from {peer_addr}");
    }
    let _info_hash = &hs[28..48];
    // Route to torrent task by info_hash — full routing deferred until
    // torrent tasks expose a stream-accept channel.
    Ok(())
}
