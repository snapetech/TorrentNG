/// Top-level engine: manages torrent task lifecycle and incoming TCP listener.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::Connection;
use sha1::{Digest, Sha1};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration, Instant};
use tracing::{info, warn};

use rt_config::Config;
use rt_db::TorrentRow;
use rt_fastresume::{FastresumeStore, PieceState};
use rt_metainfo::{parse_torrent, MagnetLink, TorrentMeta, TorrentMetaV1};
use rt_peer_wire::handshake::{Handshake, HANDSHAKE_LEN};
use rt_session::{SessionRegistry, TorrentEntry, TorrentState, TransferStats};

use crate::command::{
    CmdResult, EngineCmd, EnginePieceState, EngineStats, EngineTorrentFile, EngineTorrentMetadata,
    TorrentDiagnostic,
};
use crate::dht_task::{run_dht, DhtCommand, DhtTorrent};
use crate::metadata_task::run_metadata_task;
use crate::torrent_task::{TorrentCmd, TorrentTask};

const EVENT_ENGINE_STARTED: &str = "engine_started";
const EVENT_TORRENT_ADDED: &str = "torrent_added";
const EVENT_MAGNET_ADDED: &str = "magnet_added";
const EVENT_METADATA_RESOLVED: &str = "metadata_resolved";
const EVENT_TORRENT_RESTORED: &str = "torrent_restored";
const EVENT_TORRENT_REMOVED: &str = "torrent_removed";
const EVENT_TORRENT_PAUSED: &str = "torrent_paused";
const EVENT_TORRENT_RESUMED: &str = "torrent_resumed";
const EVENT_RECHECK_REQUESTED: &str = "check_requested";
const EVENT_REANNOUNCE_REQUESTED: &str = "tracker_reannounce_requested";
const EVENT_LABELS_UPDATED: &str = "labels_updated";
const EVENT_FIELDS_UPDATED: &str = "torrent_fields_updated";
const EVENT_TRACKERS_UPDATED: &str = "trackers_updated";
const EVENT_ENGINE_STOPPED: &str = "engine_stopped";

const JOB_KIND_RECHECK: &str = "recheck_torrent";
const JOB_STATE_QUEUED: &str = "queued";
const JOB_STATE_RUNNING: &str = "running";
const JOB_STATE_PAUSED: &str = "paused";
const JOB_STATE_CANCELLED: &str = "cancelled";
const JOB_STATE_FAILED: &str = "failed";

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
        self.add_torrent_with_labels(meta, save_path, paused, None, Vec::new())
            .await
    }

    pub async fn add_torrent_with_labels(
        &self,
        meta: TorrentMeta,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddTorrent {
                meta: Box::new(meta),
                save_path,
                paused,
                category,
                tags,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn add_magnet_with_labels(
        &self,
        magnet: MagnetLink,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::AddMagnet {
                magnet,
                save_path,
                paused,
                category,
                tags,
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

    pub async fn recheck_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::RecheckTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn pause_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::PauseJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn resume_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ResumeJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn cancel_job(&self, job_id: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::CancelJob { job_id, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn reannounce_torrent(&self, info_hash: String) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::ReannounceTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn torrent_metadata(&self, info_hash: String) -> CmdResult<EngineTorrentMetadata> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetTorrentMetadata { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn stats(&self) -> CmdResult<EngineStats> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::GetStats { reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn diagnose_torrent(&self, info_hash: String) -> CmdResult<TorrentDiagnostic> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::DiagnoseTorrent { info_hash, reply })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_labels(
        &self,
        info_hash: String,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentLabels {
                info_hash,
                category,
                add_tags,
                remove_tags,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_fields(
        &self,
        info_hash: String,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentFields {
                info_hash,
                name,
                save_path,
                reply,
            })
            .await
            .map_err(|_| "engine shut down".to_owned())?;
        rx.await.map_err(|_| "engine dropped reply".to_owned())?
    }

    pub async fn update_torrent_trackers(
        &self,
        info_hash: String,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(EngineCmd::UpdateTorrentTrackers {
                info_hash,
                trackers,
                reply,
            })
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
    db: Arc<Mutex<Connection>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    cmd_tx: mpsc::Sender<EngineCmd>,
    /// info_hash_hex → channel to the torrent task
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
    torrent_tasks: HashMap<String, JoinHandle<()>>,
    dht_tx: Option<mpsc::Sender<DhtCommand>>,
}

impl Engine {
    /// Spawn the engine, returning an EngineHandle for the API layer.
    pub async fn start(
        config: Arc<Config>,
        registry: Arc<RwLock<SessionRegistry>>,
    ) -> anyhow::Result<EngineHandle> {
        let (tx, cmd_rx) = mpsc::channel(64);
        let handle = EngineHandle { tx: tx.clone() };
        std::fs::create_dir_all(&config.daemon.session_dir)
            .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;
        std::fs::create_dir_all(torrent_blob_dir(&config))
            .with_context(|| "creating torrent metadata directory")?;
        std::fs::create_dir_all(fastresume_dir(&config))
            .with_context(|| "creating fastresume directory")?;
        let conn = Connection::open(config.db_path())
            .with_context(|| format!("opening database {:?}", config.db_path()))?;
        rt_db::migrate(&conn).context("migrating database")?;
        register_configured_storage(&conn, &config).context("registering configured storage")?;

        let dht_shutdown = if config.dht.enabled {
            let (dht_tx, dht_rx) = mpsc::channel(64);
            let dht_port = config.dht_port();
            let listen_port = config.network.listen_port;
            let bootstrap_nodes = config.dht.bootstrap_nodes.clone();
            tokio::spawn(async move {
                if let Err(e) = run_dht(dht_port, listen_port, bootstrap_nodes, dht_rx).await {
                    warn!(err = %e, "DHT task exited with error");
                }
            });
            Some(dht_tx)
        } else {
            None
        };

        let mut engine = Engine {
            config: config.clone(),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx,
            cmd_tx: tx,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: dht_shutdown,
        };
        engine.append_session_event(
            None,
            EVENT_ENGINE_STARTED,
            Some("native engine started"),
            serde_json::json!({
                "listen_port": config.network.listen_port,
                "dht_enabled": config.dht.enabled,
            }),
        );
        engine.recover_interrupted_jobs()?;
        engine.load_persisted_torrents().await?;

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
        self.shutdown_torrent_tasks().await;
        if let Some(tx) = self.dht_tx.take() {
            let _ = tx.send(DhtCommand::Shutdown).await;
        }
        self.append_session_event(
            None,
            EVENT_ENGINE_STOPPED,
            Some("native engine stopped"),
            serde_json::json!({}),
        );
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
                category,
                tags,
                reply,
            } => {
                let result = self
                    .add_torrent(*meta, save_path, paused, category, tags)
                    .await;
                let _ = reply.send(result);
            }

            EngineCmd::AddMagnet {
                magnet,
                save_path,
                paused,
                category,
                tags,
                reply,
            } => {
                let result = self
                    .add_magnet(magnet, save_path, paused, category, tags)
                    .await;
                let _ = reply.send(result);
            }

            EngineCmd::CompleteMagnet { info_hash, raw } => {
                if let Err(e) = self.complete_magnet(&info_hash, raw).await {
                    warn!(torrent = %info_hash, err = %e, "failed to complete magnet metadata");
                }
            }

            EngineCmd::RemoveTorrent {
                info_hash,
                delete_files,
                reply,
            } => {
                let result = if let Some(tx) = self.torrent_chans.remove(&info_hash) {
                    let _ = tx.send(TorrentCmd::Shutdown).await;
                    self.torrent_tasks.remove(&info_hash);
                    let mut reg = self.registry.write().await;
                    match reg.remove(&info_hash) {
                        Ok(entry) => {
                            if delete_files {
                                if let Err(e) =
                                    self.delete_payload_files(&info_hash, &entry.save_path)
                                {
                                    warn!(torrent = %info_hash, err = %e, "failed to delete torrent payload files");
                                }
                            }
                            if let Err(e) = self.delete_persisted_torrent(&info_hash) {
                                warn!(torrent = %info_hash, err = %e, "failed to delete persisted torrent");
                            }
                            self.unregister_dht_torrent(&info_hash).await;
                            self.append_session_event(
                                Some(&info_hash),
                                EVENT_TORRENT_REMOVED,
                                Some("torrent removed"),
                                serde_json::json!({ "delete_files": delete_files }),
                            );
                            Ok(())
                        }
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    Err(format!("torrent {info_hash} not found"))
                };
                let _ = reply.send(result);
            }

            EngineCmd::PauseTorrent { info_hash, reply } => {
                self.unregister_dht_torrent(&info_hash).await;
                let result = self.send_to_torrent(&info_hash, TorrentCmd::Pause).await;
                if result.is_ok() {
                    self.set_metadata_placeholder_state(&info_hash, TorrentState::Paused);
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_TORRENT_PAUSED,
                        Some("torrent paused"),
                        serde_json::json!({}),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::ResumeTorrent { info_hash, reply } => {
                let result = self.ensure_metadata_task(&info_hash).await.and_then(|_| {
                    self.torrent_chans
                        .get(&info_hash)
                        .ok_or_else(|| format!("torrent {info_hash} not found"))
                        .map(|_| ())
                });
                let result = if result.is_ok() {
                    self.send_to_torrent(&info_hash, TorrentCmd::Resume).await
                } else {
                    result
                };
                if result.is_ok() {
                    self.set_metadata_placeholder_state(&info_hash, TorrentState::MetadataPending);
                    self.register_dht_torrent_from_storage_or_hash(&info_hash)
                        .await;
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_TORRENT_RESUMED,
                        Some("torrent resumed"),
                        serde_json::json!({}),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::RecheckTorrent { info_hash, reply } => {
                let job_id = self.create_recheck_job(&info_hash);
                let result = self
                    .send_to_torrent(
                        &info_hash,
                        TorrentCmd::Recheck {
                            job_id: job_id.clone(),
                        },
                    )
                    .await;
                if result.is_ok() {
                    if let Some(job_id) = &job_id {
                        self.update_job_state(
                            job_id,
                            JOB_STATE_RUNNING,
                            None,
                            Some("recheck dispatched to torrent task"),
                        );
                    }
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_RECHECK_REQUESTED,
                        Some("torrent recheck requested"),
                        serde_json::json!({ "job_id": job_id }),
                    );
                } else if let Some(job_id) = &job_id {
                    self.update_job_state(
                        job_id,
                        JOB_STATE_FAILED,
                        result.as_ref().err().cloned(),
                        Some("recheck dispatch failed"),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::PauseJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_PAUSED).await;
                let _ = reply.send(result);
            }

            EngineCmd::ResumeJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_RUNNING).await;
                let _ = reply.send(result);
            }

            EngineCmd::CancelJob { job_id, reply } => {
                let result = self.control_recheck_job(&job_id, JOB_STATE_CANCELLED).await;
                let _ = reply.send(result);
            }

            EngineCmd::ReannounceTorrent { info_hash, reply } => {
                let result = self
                    .send_to_torrent(&info_hash, TorrentCmd::Reannounce)
                    .await;
                if result.is_ok() {
                    self.append_session_event(
                        Some(&info_hash),
                        EVENT_REANNOUNCE_REQUESTED,
                        Some("tracker reannounce requested"),
                        serde_json::json!({}),
                    );
                }
                let _ = reply.send(result);
            }

            EngineCmd::GetTorrentMetadata { info_hash, reply } => {
                let result = self
                    .load_torrent_metadata(&info_hash)
                    .map_err(|e| e.to_string());
                let _ = reply.send(result);
            }

            EngineCmd::UpdateTorrentLabels {
                info_hash,
                category,
                add_tags,
                remove_tags,
                reply,
            } => {
                let result = self
                    .update_torrent_labels_inner(&info_hash, category, add_tags, remove_tags)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentFields {
                info_hash,
                name,
                save_path,
                reply,
            } => {
                let result = self
                    .update_torrent_fields_inner(&info_hash, name, save_path)
                    .await;
                let _ = reply.send(result);
            }
            EngineCmd::UpdateTorrentTrackers {
                info_hash,
                trackers,
                reply,
            } => {
                let result = self
                    .update_torrent_trackers_inner(&info_hash, trackers)
                    .await;
                let _ = reply.send(result);
            }

            EngineCmd::GetStats { reply } => {
                let result = self.engine_stats().await;
                let _ = reply.send(result);
            }

            EngineCmd::DiagnoseTorrent { info_hash, reply } => {
                let result = self.diagnose_torrent_inner(&info_hash).await;
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
        category: Option<String>,
        tags: Vec<String>,
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
            let mut entry = TorrentEntry::new(
                info_hash_hex.clone(),
                v1.name.clone(),
                save.to_string_lossy().into_owned(),
            );
            entry.total_length = v1.total_length();
            entry.amount_left = entry.total_length;
            entry.category = normalize_category(category);
            entry.tags = normalize_tags(tags);
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

        self.save_torrent_blob(&info_hash_hex, &v1.raw)
            .map_err(|e| e.to_string())?;
        {
            let reg = self.registry.read().await;
            let entry = reg
                .get(&info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing from registry"))?;
            self.persist_entry(entry, &v1).map_err(|e| e.to_string())?;
        }

        let is_private = v1.private;
        let info_hash = v1.info_hash;
        let torrent_name = v1.name.clone();
        let _cmd_tx = self.spawn_torrent_task(info_hash_hex.clone(), v1, save, paused);
        if !paused && !is_private {
            self.register_dht_torrent(info_hash, &info_hash_hex).await;
        }
        self.append_session_event(
            Some(&info_hash_hex),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({
                "paused": paused,
                "private": is_private,
                "name": torrent_name,
            }),
        );
        info!(torrent = %info_hash_hex, paused, "torrent added");
        Ok(info_hash_hex)
    }

    async fn add_magnet(
        &mut self,
        magnet: MagnetLink,
        save_path: Option<std::path::PathBuf>,
        paused: bool,
        category: Option<String>,
        tags: Vec<String>,
    ) -> CmdResult<String> {
        let info_hash = magnet
            .info_hash_v1
            .ok_or_else(|| "only v1 btih magnets are currently supported".to_owned())?;
        let info_hash_hex = hex::encode(info_hash);
        if self.torrent_chans.contains_key(&info_hash_hex)
            || self.registry.read().await.get(&info_hash_hex).is_some()
        {
            return Err(format!("torrent {info_hash_hex} already added"));
        }

        let save = save_path.unwrap_or_else(|| self.config.storage.download_dir.clone());
        let name = magnet
            .display_name
            .clone()
            .unwrap_or_else(|| info_hash_hex.clone());
        let mut entry = TorrentEntry::new(
            info_hash_hex.clone(),
            name,
            save.to_string_lossy().into_owned(),
        );
        entry.category = normalize_category(category);
        entry.tags = normalize_tags(tags);
        entry.state = if paused {
            TorrentState::Paused
        } else {
            TorrentState::MetadataPending
        };

        {
            let mut reg = self.registry.write().await;
            reg.add(entry.clone()).map_err(|e| e.to_string())?;
        }

        let row = TorrentRow {
            info_hash: entry.info_hash.clone(),
            name: entry.name.clone(),
            total_length: 0,
            piece_length: 0,
            piece_count: 0,
            is_private: false,
            save_path: entry.save_path.clone(),
            category: entry.category.clone(),
            tags: entry.tags.clone(),
            state: entry.state.as_str().to_owned(),
            added_at: entry.added_at as i64,
            completed_at: None,
            uploaded: 0,
            downloaded: 0,
            ratio: 0.0,
            trackers: magnet.trackers.clone(),
        };
        {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        }
        {
            let _cmd_tx = self.spawn_metadata_task(
                info_hash,
                info_hash_hex.clone(),
                magnet.trackers.clone(),
                paused,
            );
        }
        if !paused {
            self.register_dht_torrent(info_hash, &info_hash_hex).await;
        }
        self.append_session_event(
            Some(&info_hash_hex),
            EVENT_MAGNET_ADDED,
            Some("magnet added as metadata pending"),
            serde_json::json!({
                "paused": paused,
                "trackers": magnet.trackers,
            }),
        );
        info!(torrent = %info_hash_hex, paused, "magnet added as metadata pending");
        Ok(info_hash_hex)
    }

    async fn complete_magnet(&mut self, info_hash_hex: &str, raw: Vec<u8>) -> CmdResult<()> {
        let meta = match parse_torrent(&raw).map_err(|e| e.to_string())? {
            TorrentMeta::V1(m) | TorrentMeta::Hybrid(m, _) => m,
            TorrentMeta::V2(_) => return Err("pure v2 metadata is not yet supported".to_owned()),
        };
        let fetched_hash = hex::encode(meta.info_hash);
        if fetched_hash != info_hash_hex {
            return Err(format!(
                "fetched metadata hash {fetched_hash} does not match magnet {info_hash_hex}"
            ));
        }

        let (save, category, tags) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            (
                PathBuf::from(&entry.save_path),
                entry.category.clone(),
                entry.tags.clone(),
            )
        };

        self.save_torrent_blob(info_hash_hex, &raw)
            .map_err(|e| e.to_string())?;
        {
            let mut reg = self.registry.write().await;
            let entry = reg
                .get_mut(info_hash_hex)
                .ok_or_else(|| format!("metadata-pending torrent {info_hash_hex} not found"))?;
            entry.name = meta.name.clone();
            entry.total_length = meta.total_length();
            entry.amount_left = meta.total_length();
            entry.category = category;
            entry.tags = tags;
            let _ = entry.transition(TorrentState::Downloading);
        }
        {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash_hex)
                .ok_or_else(|| format!("torrent {info_hash_hex} missing after metadata update"))?;
            self.persist_entry(entry, &meta)
                .map_err(|e| e.to_string())?;
        }

        if let Some(old_tx) = self.torrent_chans.remove(info_hash_hex) {
            let _ = old_tx.send(TorrentCmd::Shutdown).await;
        }
        if let Some(old_task) = self.torrent_tasks.remove(info_hash_hex) {
            tokio::spawn(async move {
                let _ = timeout(Duration::from_secs(10), old_task).await;
            });
        }
        let is_private = meta.private;
        let info_hash = meta.info_hash;
        let torrent_name = meta.name.clone();
        let total_length = meta.total_length();
        let _tx = self.spawn_torrent_task(info_hash_hex.to_owned(), meta, save, false);
        if !is_private {
            self.register_dht_torrent(info_hash, info_hash_hex).await;
        }
        self.append_session_event(
            Some(info_hash_hex),
            EVENT_METADATA_RESOLVED,
            Some("magnet metadata resolved"),
            serde_json::json!({
                "name": torrent_name,
                "total_length": total_length,
                "private": is_private,
            }),
        );
        info!(torrent = %info_hash_hex, "magnet metadata completed");
        Ok(())
    }

    fn spawn_torrent_task(
        &mut self,
        info_hash_hex: String,
        meta: TorrentMetaV1,
        save: PathBuf,
        paused: bool,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let task = TorrentTask::new(
            meta,
            save,
            paused,
            Arc::clone(&self.registry),
            Arc::clone(&self.db),
            cmd_rx,
            fastresume_dir(&self.config),
            self.config.network.max_peers,
            self.config.network.listen_port,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            self.config.tracker.min_interval_secs,
        );
        let handle = tokio::spawn(task.run());
        self.torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.torrent_tasks.insert(info_hash_hex, handle);
        cmd_tx
    }

    fn spawn_metadata_task(
        &mut self,
        info_hash: [u8; 20],
        info_hash_hex: String,
        trackers: Vec<String>,
        paused: bool,
    ) -> mpsc::Sender<TorrentCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<TorrentCmd>(32);
        let handle = tokio::spawn(run_metadata_task(
            info_hash,
            info_hash_hex.clone(),
            trackers,
            cmd_rx,
            self.cmd_tx.clone(),
            self.config.network.listen_port,
            self.config.network.max_peers,
            self.config.tracker.http_timeout_secs,
            self.config.tracker.udp_timeout_secs,
            paused,
        ));
        self.torrent_chans
            .insert(info_hash_hex.clone(), cmd_tx.clone());
        self.torrent_tasks.insert(info_hash_hex, handle);
        cmd_tx
    }

    async fn shutdown_torrent_tasks(&mut self) {
        let task_count = self.torrent_chans.len();
        for tx in self.torrent_chans.values() {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
        self.torrent_chans.clear();

        let timeout_secs = self.config.daemon.shutdown_timeout_secs.max(1);
        let timeout_budget = Duration::from_secs(timeout_secs);
        let deadline = Instant::now() + timeout_budget;
        let mut timed_out = false;

        for (info_hash, mut task) in std::mem::take(&mut self.torrent_tasks) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                timed_out = true;
                task.abort();
                warn!(
                    torrent = %info_hash,
                    timeout_secs,
                    "aborted torrent task after shutdown deadline"
                );
                continue;
            };

            match timeout(remaining, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if !e.is_cancelled() {
                        warn!(torrent = %info_hash, err = %e, "torrent task failed during shutdown");
                    }
                }
                Err(_) => {
                    timed_out = true;
                    task.abort();
                    warn!(
                        torrent = %info_hash,
                        timeout_secs,
                        "aborted torrent task after shutdown deadline"
                    );
                }
            }
        }

        if !timed_out {
            info!(tasks = task_count, "torrent tasks stopped cleanly");
        }
    }

    async fn load_persisted_torrents(&mut self) -> anyhow::Result<()> {
        let rows = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::list_all(&db)?
        };

        for row in rows {
            if self.is_metadata_placeholder_row(&row) {
                let paused = state_from_str(&row.state) == TorrentState::Paused;
                let entry = entry_from_row(&row);
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(torrent = %row.info_hash, err = %e, "failed to restore metadata-pending registry entry");
                }
                drop(reg);
                if let Ok(info_hash) = parse_info_hash_hex(&row.info_hash) {
                    let _tx = self.spawn_metadata_task(
                        info_hash,
                        row.info_hash.clone(),
                        row.trackers.clone(),
                        paused,
                    );
                    if !paused {
                        self.register_dht_torrent(info_hash, &row.info_hash).await;
                    }
                    self.append_session_event(
                        Some(&row.info_hash),
                        EVENT_TORRENT_RESTORED,
                        Some("metadata-pending torrent restored"),
                        serde_json::json!({
                            "state": row.state,
                            "metadata_pending": true,
                        }),
                    );
                }
                continue;
            }
            let blob_path = torrent_blob_path(&self.config, &row.info_hash);
            let raw = match std::fs::read(&blob_path) {
                Ok(raw) => raw,
                Err(e) => {
                    warn!(
                        torrent = %row.info_hash,
                        path = %blob_path.display(),
                        err = %e,
                        "persisted torrent metadata missing"
                    );
                    continue;
                }
            };
            let meta = match parse_torrent(&raw) {
                Ok(TorrentMeta::V1(m)) | Ok(TorrentMeta::Hybrid(m, _)) => m,
                Ok(TorrentMeta::V2(_)) => {
                    warn!(torrent = %row.info_hash, "pure v2 persisted torrent is unsupported");
                    continue;
                }
                Err(e) => {
                    warn!(torrent = %row.info_hash, err = %e, "failed to parse persisted torrent");
                    continue;
                }
            };
            let info_hash_hex = meta
                .info_hash
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            if info_hash_hex != row.info_hash {
                warn!(
                    row_hash = %row.info_hash,
                    meta_hash = %info_hash_hex,
                    "persisted torrent hash mismatch"
                );
                continue;
            }

            let entry = entry_from_row(&row);
            {
                let mut reg = self.registry.write().await;
                if let Err(e) = reg.add(entry) {
                    warn!(torrent = %row.info_hash, err = %e, "failed to restore registry entry");
                    continue;
                }
            }

            let state = state_from_str(&row.state);
            let paused = !matches!(state, TorrentState::Downloading);
            let is_private = meta.private;
            let info_hash = meta.info_hash;
            let _tx = self.spawn_torrent_task(
                row.info_hash.clone(),
                meta,
                PathBuf::from(&row.save_path),
                paused,
            );
            if !paused && !is_private {
                self.register_dht_torrent(info_hash, &row.info_hash).await;
            }
            self.append_session_event(
                Some(&row.info_hash),
                EVENT_TORRENT_RESTORED,
                Some("torrent restored from database"),
                serde_json::json!({
                    "state": row.state,
                    "private": is_private,
                }),
            );
            info!(torrent = %row.info_hash, state = %row.state, paused, "restored persisted torrent");
        }
        Ok(())
    }

    fn recover_interrupted_jobs(&self) -> anyhow::Result<()> {
        let now = unix_now_i64();
        let db = self.db.lock().expect("database mutex poisoned");
        let jobs = rt_db::list_active_jobs(&db)?;
        for mut job in jobs {
            let previous_state = job.state.clone();
            let recovered_state = match previous_state.as_str() {
                JOB_STATE_RUNNING | "cancelling" => Some(JOB_STATE_PAUSED),
                _ => None,
            };
            let Some(recovered_state) = recovered_state else {
                continue;
            };
            job.state = recovered_state.to_owned();
            job.updated_at = now;
            rt_db::upsert_job(&db, &job)?;
            rt_db::append_job_event(
                &db,
                &rt_db::JobEventRow {
                    event_id: None,
                    job_id: job.job_id.clone(),
                    occurred_at: now,
                    kind: "job_recovered".to_owned(),
                    message: Some("job recovered after engine restart".to_owned()),
                    payload: serde_json::json!({
                        "state": recovered_state,
                        "previous_state": previous_state,
                    })
                    .to_string(),
                },
            )?;
        }
        Ok(())
    }

    fn persist_entry(&self, entry: &TorrentEntry, meta: &TorrentMetaV1) -> anyhow::Result<()> {
        let row = row_from_entry(entry, meta);
        let mut db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row)?;
        persist_torrent_files(&mut db, &entry.info_hash, meta)?;
        Ok(())
    }

    fn save_torrent_blob(&self, info_hash: &str, raw: &[u8]) -> anyhow::Result<()> {
        let path = torrent_blob_path(&self.config, info_hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, raw)?;
        Ok(())
    }

    fn delete_persisted_torrent(&self, info_hash: &str) -> anyhow::Result<()> {
        {
            let db = self.db.lock().expect("database mutex poisoned");
            let _ = rt_db::delete(&db, info_hash)?;
        }
        match std::fs::remove_file(torrent_blob_path(&self.config, info_hash)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        FastresumeStore::new(fastresume_dir(&self.config)).delete(info_hash)?;
        Ok(())
    }

    fn delete_payload_files(&self, info_hash: &str, save_path: &str) -> anyhow::Result<()> {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let raw = std::fs::read(&blob_path).with_context(|| {
            format!("reading persisted torrent metadata {}", blob_path.display())
        })?;
        let meta = match parse_torrent(&raw)? {
            TorrentMeta::V1(m) | TorrentMeta::Hybrid(m, _) => m,
            TorrentMeta::V2(_) => anyhow::bail!("pure v2 torrents are not supported"),
        };
        let root = PathBuf::from(save_path);
        for file in &meta.files {
            let path = file.path.resolve(&root);
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    prune_empty_dirs(path.parent(), &root)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn load_torrent_metadata(&self, info_hash: &str) -> anyhow::Result<EngineTorrentMetadata> {
        let blob_path = torrent_blob_path(&self.config, info_hash);
        let raw = match std::fs::read(&blob_path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let row = {
                    let db = self.db.lock().expect("database mutex poisoned");
                    rt_db::get(&db, info_hash)?
                };
                if self.is_metadata_placeholder_row(&row) {
                    return Ok(metadata_from_placeholder_row(&row));
                }
                return Err(e).with_context(|| {
                    format!("reading persisted torrent metadata {}", blob_path.display())
                });
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("reading persisted torrent metadata {}", blob_path.display())
                });
            }
        };
        let meta = match parse_torrent(&raw)? {
            TorrentMeta::V1(m) | TorrentMeta::Hybrid(m, _) => m,
            TorrentMeta::V2(_) => anyhow::bail!("pure v2 torrents are not supported"),
        };
        let mut metadata = metadata_from_v1(&meta);
        if let Ok(row) = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash)
        } {
            if !row.trackers.is_empty() {
                metadata.trackers = row.trackers;
            }
        }
        if let Ok(hash) = decode_info_hash(info_hash) {
            if let Ok(state) = FastresumeStore::new(fastresume_dir(&self.config)).load(info_hash) {
                if state.validate(&hash, metadata.piece_count as u32).is_ok() {
                    let mut pieces = state
                        .pieces
                        .iter()
                        .map(|piece| match piece {
                            PieceState::Valid => EnginePieceState::Complete,
                            PieceState::Invalid | PieceState::Unknown | PieceState::Missing => {
                                EnginePieceState::Missing
                            }
                        })
                        .collect::<Vec<_>>();
                    for partial in state.partial_pieces {
                        if let Some(piece) = pieces.get_mut(partial.piece as usize) {
                            if !partial.received_blocks.is_empty() {
                                *piece = EnginePieceState::Partial;
                            }
                        }
                    }
                    metadata.piece_states = pieces;
                }
            }
        }
        Ok(metadata)
    }

    async fn update_torrent_labels_inner(
        &self,
        info_hash: &str,
        category: Option<Option<String>>,
        add_tags: Vec<String>,
        remove_tags: Vec<String>,
    ) -> CmdResult<()> {
        let mut reg = self.registry.write().await;
        let entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;

        if let Some(category) = category {
            entry.category = category.and_then(|value| normalize_category(Some(value)));
        }
        for tag in normalize_tags(add_tags) {
            if !entry.tags.contains(&tag) {
                entry.tags.push(tag);
            }
        }
        let remove_tags = normalize_tags(remove_tags);
        if !remove_tags.is_empty() {
            entry.tags.retain(|tag| !remove_tags.contains(tag));
        }
        let row = match load_v1_from_blob(&self.config, info_hash) {
            Ok(meta) => row_from_entry(entry, &meta),
            Err(_) => {
                let db = self.db.lock().expect("database mutex poisoned");
                let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
                row.category = entry.category.clone();
                row.tags = entry.tags.clone();
                row
            }
        };
        drop(reg);

        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            EVENT_LABELS_UPDATED,
            Some("torrent labels updated"),
            serde_json::json!({
                "category": row.category,
                "tags": row.tags,
            }),
        );
        Ok(())
    }

    async fn update_torrent_fields_inner(
        &self,
        info_hash: &str,
        name: Option<String>,
        save_path: Option<std::path::PathBuf>,
    ) -> CmdResult<()> {
        let mut reg = self.registry.write().await;
        let entry = reg
            .get_mut(info_hash)
            .ok_or_else(|| format!("torrent {info_hash} not found"))?;

        if let Some(name) = normalize_optional_text(name) {
            entry.name = name;
        }
        if let Some(save_path) = save_path {
            entry.save_path = save_path.to_string_lossy().to_string();
        }

        let row = match load_v1_from_blob(&self.config, info_hash) {
            Ok(meta) => row_from_entry(entry, &meta),
            Err(_) => {
                let db = self.db.lock().expect("database mutex poisoned");
                let mut row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
                row.name = entry.name.clone();
                row.save_path = entry.save_path.clone();
                row
            }
        };
        drop(reg);

        let db = self.db.lock().expect("database mutex poisoned");
        rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
        drop(db);
        self.append_session_event(
            Some(info_hash),
            EVENT_FIELDS_UPDATED,
            Some("torrent fields updated"),
            serde_json::json!({
                "name": row.name,
                "save_path": row.save_path,
            }),
        );
        Ok(())
    }

    async fn update_torrent_trackers_inner(
        &self,
        info_hash: &str,
        trackers: Vec<String>,
    ) -> CmdResult<()> {
        let trackers = normalize_tracker_urls(trackers);
        let mut row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash).map_err(|e| e.to_string())?
        };
        row.trackers = trackers.clone();
        let tracker_rows = trackers
            .iter()
            .enumerate()
            .map(|(idx, url)| rt_db::TorrentTrackerRow {
                info_hash: info_hash.to_owned(),
                tracker_index: idx as i64,
                tier: idx as i64,
                url: url.clone(),
                status: "pending".to_owned(),
                last_announce_at: None,
                next_announce_at: None,
                last_success_at: None,
                failure_reason: None,
                warning_message: None,
                seeders: None,
                leechers: None,
                completed: None,
                uploaded: row.uploaded,
                downloaded: row.downloaded,
                left_bytes: row.total_length.saturating_sub(row.downloaded).max(0),
            })
            .collect::<Vec<_>>();

        {
            let mut db = self.db.lock().expect("database mutex poisoned");
            rt_db::upsert(&db, &row).map_err(|e| e.to_string())?;
            rt_db::replace_torrent_trackers(&mut db, info_hash, &tracker_rows)
                .map_err(|e| e.to_string())?;
        }
        self.append_session_event(
            Some(info_hash),
            EVENT_TRACKERS_UPDATED,
            Some("torrent trackers updated"),
            serde_json::json!({ "trackers": trackers }),
        );
        Ok(())
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

    async fn engine_stats(&self) -> CmdResult<EngineStats> {
        let mut stats = EngineStats::default();
        {
            let reg = self.registry.read().await;
            for entry in reg.iter() {
                stats.torrents_total += 1;
                stats.bytes_uploaded = stats.bytes_uploaded.saturating_add(entry.stats.uploaded);
                stats.bytes_downloaded = stats
                    .bytes_downloaded
                    .saturating_add(entry.stats.downloaded);
                stats.bytes_left = stats.bytes_left.saturating_add(entry.amount_left);
                match entry.state {
                    TorrentState::Seeding => stats.torrents_seeding += 1,
                    TorrentState::Downloading => stats.torrents_downloading += 1,
                    TorrentState::Paused | TorrentState::Stopped => stats.torrents_paused += 1,
                    TorrentState::Checking => stats.torrents_checking += 1,
                    TorrentState::MetadataPending => stats.torrents_metadata_pending += 1,
                    TorrentState::Queued => stats.torrents_queued += 1,
                    TorrentState::Error => stats.torrents_error += 1,
                }
            }
        }
        let db = self.db.lock().expect("database mutex poisoned");
        stats.jobs_active = rt_db::list_active_jobs(&db)
            .map_err(|e| e.to_string())?
            .len() as u64;
        let trackers = rt_db::list_all_torrent_trackers(&db).map_err(|e| e.to_string())?;
        stats.trackers_total = trackers.len() as u64;
        for tracker in trackers {
            match tracker.status.as_str() {
                "working" => stats.trackers_working += 1,
                "warning" => stats.trackers_warning += 1,
                "error" => stats.trackers_error += 1,
                _ => {}
            }
        }
        Ok(stats)
    }

    async fn diagnose_torrent_inner(&self, info_hash: &str) -> CmdResult<TorrentDiagnostic> {
        let (state, bytes_left) = {
            let reg = self.registry.read().await;
            let entry = reg
                .get(info_hash)
                .ok_or_else(|| format!("torrent {info_hash} not found"))?;
            (entry.state, entry.amount_left)
        };
        let (is_private, trackers, active_jobs) = {
            let db = self.db.lock().expect("database mutex poisoned");
            let row = rt_db::get(&db, info_hash).map_err(|e| e.to_string())?;
            let trackers = rt_db::list_torrent_trackers(&db, info_hash).unwrap_or_default();
            let active_jobs = rt_db::list_active_jobs(&db)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|job| job.affected_torrents.iter().any(|hash| hash == info_hash))
                .count();
            (row.is_private, trackers, active_jobs)
        };
        let tracker_errors = trackers
            .iter()
            .filter(|tracker| tracker.status == "error")
            .count();
        let tracker_warnings = trackers
            .iter()
            .filter(|tracker| tracker.status == "warning")
            .count();
        let mut reasons = Vec::new();
        let mut next_actions = Vec::new();
        if state == TorrentState::Seeding && bytes_left == 0 {
            reasons.push("torrent is already seeding".to_owned());
        } else {
            match state {
                TorrentState::Paused | TorrentState::Stopped => {
                    reasons.push("torrent is paused or stopped".to_owned());
                    next_actions.push("resume the torrent".to_owned());
                }
                TorrentState::Checking => {
                    reasons.push("torrent is currently checking pieces".to_owned());
                    next_actions.push("wait for the active recheck job to finish".to_owned());
                }
                TorrentState::MetadataPending => {
                    reasons.push("torrent is waiting for metadata".to_owned());
                    next_actions
                        .push("wait for metadata peers or add the .torrent file".to_owned());
                }
                TorrentState::Downloading | TorrentState::Queued => {
                    reasons.push(format!("{bytes_left} bytes are still missing"));
                }
                TorrentState::Error => {
                    reasons.push("torrent is in an error state".to_owned());
                    next_actions.push("inspect tracker and storage errors".to_owned());
                }
                TorrentState::Seeding => {}
            }
            if active_jobs > 0 {
                reasons.push(format!("{active_jobs} active job(s) affect this torrent"));
            }
            if tracker_errors > 0 {
                reasons.push(format!("{tracker_errors} tracker(s) are in error state"));
                next_actions.push("check tracker failure_reason values".to_owned());
            }
            if tracker_warnings > 0 {
                reasons.push(format!("{tracker_warnings} tracker(s) reported warnings"));
            }
            if is_private && trackers.is_empty() {
                reasons.push("private torrent has no persisted trackers".to_owned());
                next_actions.push("add a private tracker before expecting peers".to_owned());
            }
        }
        if next_actions.is_empty() && state != TorrentState::Seeding {
            next_actions.push("inspect torrent files, trackers, and active jobs".to_owned());
        }
        Ok(TorrentDiagnostic {
            info_hash: info_hash.to_owned(),
            state: state.as_str().to_owned(),
            is_private,
            bytes_left,
            active_jobs,
            tracker_errors,
            tracker_warnings,
            reasons,
            next_actions,
        })
    }

    async fn control_recheck_job(&self, job_id: &str, target_state: &str) -> CmdResult<()> {
        let job = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get_job(&db, job_id).map_err(|e| e.to_string())?
        };
        if job.kind != JOB_KIND_RECHECK {
            return Err(format!("job {job_id} is not a recheck job"));
        }
        if job.finished_at.is_some()
            || matches!(
                job.state.as_str(),
                JOB_STATE_CANCELLED | "completed" | JOB_STATE_FAILED
            )
        {
            return Err(format!("job {job_id} is already terminal"));
        }
        let info_hash = job
            .affected_torrents
            .first()
            .ok_or_else(|| format!("job {job_id} has no target torrent"))?
            .clone();
        match target_state {
            JOB_STATE_PAUSED => {
                self.send_to_torrent(&info_hash, TorrentCmd::Pause).await?;
                self.update_job_state(job_id, JOB_STATE_PAUSED, None, Some("recheck job paused"));
            }
            JOB_STATE_RUNNING => {
                self.send_to_torrent(
                    &info_hash,
                    TorrentCmd::Recheck {
                        job_id: Some(job_id.to_owned()),
                    },
                )
                .await?;
                self.update_job_state(job_id, JOB_STATE_RUNNING, None, Some("recheck job resumed"));
            }
            JOB_STATE_CANCELLED => {
                self.send_to_torrent(
                    &info_hash,
                    TorrentCmd::CancelJob {
                        job_id: job_id.to_owned(),
                    },
                )
                .await?;
                self.update_job_state(
                    job_id,
                    JOB_STATE_CANCELLED,
                    None,
                    Some("recheck job cancelled"),
                );
            }
            _ => return Err(format!("unsupported job state {target_state}")),
        }
        Ok(())
    }

    async fn ensure_metadata_task(&mut self, info_hash_hex: &str) -> CmdResult<()> {
        if self.torrent_chans.contains_key(info_hash_hex) {
            return Ok(());
        }
        let row = {
            let db = self.db.lock().expect("database mutex poisoned");
            rt_db::get(&db, info_hash_hex).map_err(|e| e.to_string())?
        };
        if !self.is_metadata_placeholder_row(&row) {
            return Err(format!("torrent {info_hash_hex} not running"));
        }
        let info_hash =
            parse_info_hash_hex(info_hash_hex).map_err(|_| "invalid info hash".to_owned())?;
        let _tx = self.spawn_metadata_task(
            info_hash,
            info_hash_hex.to_owned(),
            row.trackers,
            state_from_str(&row.state) == TorrentState::Paused,
        );
        Ok(())
    }

    async fn register_dht_torrent(&self, info_hash: [u8; 20], info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Some(cmd_tx) = self.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        let _ = dht_tx
            .send(DhtCommand::AddTorrent(DhtTorrent { info_hash, cmd_tx }))
            .await;
    }

    async fn register_dht_torrent_from_blob(&self, info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Some(cmd_tx) = self.torrent_chans.get(info_hash_hex).cloned() else {
            return;
        };
        let raw = match std::fs::read(torrent_blob_path(&self.config, info_hash_hex)) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(torrent = %info_hash_hex, err = %e, "failed to load torrent metadata for DHT registration");
                return;
            }
        };
        let meta = match parse_torrent(&raw) {
            Ok(TorrentMeta::V1(m)) | Ok(TorrentMeta::Hybrid(m, _)) => m,
            _ => return,
        };
        if meta.private {
            return;
        }
        let _ = dht_tx
            .send(DhtCommand::AddTorrent(DhtTorrent {
                info_hash: meta.info_hash,
                cmd_tx,
            }))
            .await;
    }

    async fn register_dht_torrent_from_storage_or_hash(&self, info_hash_hex: &str) {
        if std::fs::metadata(torrent_blob_path(&self.config, info_hash_hex)).is_ok() {
            self.register_dht_torrent_from_blob(info_hash_hex).await;
            return;
        }
        match parse_info_hash_hex(info_hash_hex) {
            Ok(info_hash) => self.register_dht_torrent(info_hash, info_hash_hex).await,
            Err(()) => {
                warn!(torrent = %info_hash_hex, "failed to parse info hash for DHT registration")
            }
        }
    }

    fn is_metadata_placeholder_row(&self, row: &TorrentRow) -> bool {
        if state_from_str(&row.state) == TorrentState::MetadataPending {
            return true;
        }
        state_from_str(&row.state) == TorrentState::Paused
            && row.total_length == 0
            && row.piece_count == 0
            && std::fs::metadata(torrent_blob_path(&self.config, &row.info_hash)).is_err()
    }

    fn set_metadata_placeholder_state(&self, info_hash: &str, state: TorrentState) {
        let mut row = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get(&db, info_hash) {
                Ok(row) => row,
                Err(_) => return,
            }
        };
        if !self.is_metadata_placeholder_row(&row) {
            return;
        }
        row.state = state.as_str().to_owned();
        {
            let db = self.db.lock().expect("database mutex poisoned");
            let _ = rt_db::upsert(&db, &row);
        }
        if let Ok(mut registry) = self.registry.try_write() {
            if let Some(entry) = registry.get_mut(info_hash) {
                entry.state = state;
            }
        }
    }

    async fn unregister_dht_torrent(&self, info_hash_hex: &str) {
        let Some(dht_tx) = &self.dht_tx else {
            return;
        };
        let Ok(info_hash) = parse_info_hash_hex(info_hash_hex) else {
            return;
        };
        let _ = dht_tx.send(DhtCommand::RemoveTorrent(info_hash)).await;
    }

    fn append_session_event(
        &self,
        info_hash: Option<&str>,
        kind: &str,
        message: Option<&str>,
        payload: serde_json::Value,
    ) {
        let event = rt_db::SessionEventRow {
            event_id: None,
            occurred_at: unix_now_i64(),
            info_hash: info_hash.map(str::to_owned),
            kind: kind.to_owned(),
            message: message.map(str::to_owned),
            payload: payload.to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::append_session_event(&db, &event) {
            warn!(kind, err = %e, "failed to append session event");
        }
    }

    fn create_recheck_job(&self, info_hash: &str) -> Option<String> {
        let now = unix_now_i64();
        let total = self
            .load_torrent_metadata(info_hash)
            .map(|meta| meta.piece_count as i64)
            .unwrap_or(0);
        let job_id = format!("recheck-{info_hash}-{now}");
        let job = rt_db::JobRow {
            job_id: job_id.clone(),
            kind: JOB_KIND_RECHECK.to_owned(),
            state: JOB_STATE_QUEUED.to_owned(),
            dry_run: false,
            affected_torrents: vec![info_hash.to_owned()],
            total,
            done: 0,
            checkpoint: 0,
            file_index: Some(0),
            piece_index: Some(0),
            byte_offset: Some(0),
            verified_bytes: 0,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: now,
            started_at: None,
            updated_at: now,
            finished_at: None,
        };
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.clone(),
            occurred_at: now,
            kind: "job_queued".to_owned(),
            message: Some("recheck queued".to_owned()),
            payload: serde_json::json!({ "info_hash": info_hash }).to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(torrent = %info_hash, err = %e, "failed to persist recheck job");
            return None;
        }
        if let Err(e) = rt_db::append_job_event(&db, &event) {
            warn!(job_id = %job_id, err = %e, "failed to append recheck job event");
        }
        Some(job_id)
    }

    fn update_job_state(
        &self,
        job_id: &str,
        state: &str,
        error: Option<String>,
        message: Option<&str>,
    ) {
        let now = unix_now_i64();
        let mut job = {
            let db = self.db.lock().expect("database mutex poisoned");
            match rt_db::get_job(&db, job_id) {
                Ok(job) => job,
                Err(e) => {
                    warn!(job_id, err = %e, "failed to load job for state update");
                    return;
                }
            }
        };
        job.state = state.to_owned();
        job.error = error;
        job.updated_at = now;
        if state == JOB_STATE_RUNNING && job.started_at.is_none() {
            job.started_at = Some(now);
        }
        if matches!(state, JOB_STATE_CANCELLED | JOB_STATE_FAILED) {
            job.finished_at = Some(now);
        }
        let event = rt_db::JobEventRow {
            event_id: None,
            job_id: job_id.to_owned(),
            occurred_at: now,
            kind: format!("job_{state}"),
            message: message.map(str::to_owned),
            payload: serde_json::json!({ "state": state }).to_string(),
        };
        let db = self.db.lock().expect("database mutex poisoned");
        if let Err(e) = rt_db::upsert_job(&db, &job) {
            warn!(job_id, err = %e, "failed to persist job state");
            return;
        }
        if let Err(e) = rt_db::append_job_event(&db, &event) {
            warn!(job_id, err = %e, "failed to append job state event");
        }
        if state == JOB_STATE_RUNNING {
            let started = rt_db::JobEventRow {
                event_id: None,
                job_id: job_id.to_owned(),
                occurred_at: now,
                kind: "check_started".to_owned(),
                message: Some("recheck started".to_owned()),
                payload: serde_json::json!({ "state": state }).to_string(),
            };
            if let Err(e) = rt_db::append_job_event(&db, &started) {
                warn!(job_id, err = %e, "failed to append recheck start event");
            }
        }
    }
}

pub(crate) fn row_from_entry(entry: &TorrentEntry, meta: &TorrentMetaV1) -> TorrentRow {
    TorrentRow {
        info_hash: entry.info_hash.clone(),
        name: entry.name.clone(),
        total_length: meta.total_length() as i64,
        piece_length: meta.piece_length as i64,
        piece_count: meta.pieces.len() as i64,
        is_private: meta.private,
        save_path: entry.save_path.clone(),
        category: entry.category.clone(),
        tags: entry.tags.clone(),
        state: entry.state.as_str().to_owned(),
        added_at: entry.added_at as i64,
        completed_at: entry.completed_at.map(|t| t as i64),
        uploaded: entry.stats.uploaded as i64,
        downloaded: entry.stats.downloaded as i64,
        ratio: entry.stats.ratio(),
        trackers: meta.all_trackers(),
    }
}

fn persist_torrent_files(
    db: &mut Connection,
    info_hash: &str,
    meta: &TorrentMetaV1,
) -> anyhow::Result<()> {
    let rows: Vec<_> = meta
        .files
        .iter()
        .map(|file| rt_db::TorrentFileRow {
            info_hash: info_hash.to_owned(),
            file_index: file.index as i64,
            path: file.path.as_display(),
            length: file.length as i64,
            offset: file.offset as i64,
            priority: 1,
            wanted: true,
            completed_bytes: 0,
        })
        .collect();
    rt_db::replace_torrent_files(db, info_hash, &rows)?;
    Ok(())
}

fn entry_from_row(row: &TorrentRow) -> TorrentEntry {
    TorrentEntry {
        handle: Default::default(),
        info_hash: row.info_hash.clone(),
        name: row.name.clone(),
        save_path: row.save_path.clone(),
        total_length: row.total_length.max(0) as u64,
        amount_left: if state_from_str(&row.state) == TorrentState::Seeding {
            0
        } else {
            (row.total_length.max(0) as u64).saturating_sub(row.downloaded.max(0) as u64)
        },
        state: state_from_str(&row.state),
        stats: TransferStats {
            uploaded: row.uploaded.max(0) as u64,
            downloaded: row.downloaded.max(0) as u64,
        },
        added_at: row.added_at.max(0) as u64,
        completed_at: row.completed_at.map(|t| t.max(0) as u64),
        category: row.category.clone(),
        tags: row.tags.clone(),
        error_message: None,
    }
}

fn state_from_str(state: &str) -> TorrentState {
    match state {
        "checking" => TorrentState::Checking,
        "metadata_pending" => TorrentState::MetadataPending,
        "seeding" => TorrentState::Seeding,
        "downloading" => TorrentState::Downloading,
        "paused" => TorrentState::Paused,
        "queued" => TorrentState::Queued,
        "error" => TorrentState::Error,
        _ => TorrentState::Stopped,
    }
}

fn torrent_blob_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("torrents")
}

fn torrent_blob_path(config: &Config, info_hash: &str) -> PathBuf {
    torrent_blob_dir(config).join(format!("{info_hash}.torrent"))
}

fn load_v1_from_blob(config: &Config, info_hash: &str) -> anyhow::Result<TorrentMetaV1> {
    let raw = std::fs::read(torrent_blob_path(config, info_hash))?;
    match parse_torrent(&raw)? {
        TorrentMeta::V1(m) | TorrentMeta::Hybrid(m, _) => Ok(m),
        TorrentMeta::V2(_) => anyhow::bail!("pure v2 torrents are not supported"),
    }
}

fn fastresume_dir(config: &Config) -> PathBuf {
    config.daemon.session_dir.join("fastresume")
}

fn register_configured_storage(conn: &Connection, config: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.storage.download_dir)?;
    let path = normalized_path_string(&config.storage.download_dir);
    let now = unix_now_i64();
    let root_id = stable_id("root", &path);
    let mount_id = stable_mount_id(&config.storage.download_dir);
    rt_db::upsert_storage_root(
        conn,
        &rt_db::StorageRootRow {
            root_id,
            path: path.clone(),
            profile: "auto".to_owned(),
            created_at: now,
        },
    )?;
    rt_db::upsert_mount(
        conn,
        &rt_db::MountRow {
            mount_id,
            path,
            fs_type: Some("unknown".to_owned()),
            device: storage_device_id(&config.storage.download_dir),
            queue_depth: 1,
            read_concurrency: 1,
            write_concurrency: 1,
            updated_at: now,
        },
    )?;
    Ok(())
}

fn normalized_path_string(path: &std::path::Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn stable_mount_id(path: &std::path::Path) -> String {
    let key = storage_device_id(path).unwrap_or_else(|| normalized_path_string(path));
    stable_id("mount", &key)
}

fn stable_id(prefix: &str, value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    format!("{prefix}-{}", hex::encode(&digest[..10]))
}

#[cfg(unix)]
fn storage_device_id(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    std::fs::metadata(path)
        .ok()
        .map(|metadata| format!("dev:{}", metadata.dev()))
}

#[cfg(not(unix))]
fn storage_device_id(_path: &std::path::Path) -> Option<String> {
    None
}

fn parse_info_hash_hex(info_hash: &str) -> Result<[u8; 20], ()> {
    if info_hash.len() != 40 {
        return Err(());
    }
    let mut out = [0u8; 20];
    for (idx, chunk) in info_hash.as_bytes().chunks_exact(2).enumerate() {
        let hex = std::str::from_utf8(chunk).map_err(|_| ())?;
        out[idx] = u8::from_str_radix(hex, 16).map_err(|_| ())?;
    }
    Ok(out)
}

fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn normalize_category(category: Option<String>) -> Option<String> {
    category
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_owned();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

fn normalize_tracker_urls(trackers: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for tracker in trackers {
        let tracker = tracker.trim().to_owned();
        if !tracker.is_empty() && !out.contains(&tracker) {
            out.push(tracker);
        }
    }
    out
}

fn metadata_from_v1(meta: &TorrentMetaV1) -> EngineTorrentMetadata {
    EngineTorrentMetadata {
        piece_length: meta.piece_length,
        piece_count: meta.pieces.len(),
        piece_hashes: meta.pieces.iter().map(hex::encode).collect(),
        piece_states: vec![EnginePieceState::Missing; meta.pieces.len()],
        is_private: meta.private,
        trackers: meta.all_trackers(),
        files: meta
            .files
            .iter()
            .map(|file| EngineTorrentFile {
                index: file.index,
                path: file.path.as_display(),
                length: file.length,
            })
            .collect(),
    }
}

fn metadata_from_placeholder_row(row: &TorrentRow) -> EngineTorrentMetadata {
    EngineTorrentMetadata {
        piece_length: 0,
        piece_count: 0,
        piece_hashes: Vec::new(),
        piece_states: Vec::new(),
        is_private: row.is_private,
        trackers: row.trackers.clone(),
        files: Vec::new(),
    }
}

fn decode_info_hash(info_hash: &str) -> anyhow::Result<[u8; 20]> {
    let bytes = hex::decode(info_hash)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 20-byte info hash"))
}

fn prune_empty_dirs(
    mut dir: Option<&std::path::Path>,
    root: &std::path::Path,
) -> anyhow::Result<()> {
    while let Some(current) = dir {
        if current == root {
            break;
        }
        match std::fs::remove_dir(current) {
            Ok(()) => {
                dir = current.parent();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::DirectoryNotEmpty
                        | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rt_bencode::{encode, BValue};
    use rt_metainfo::TorrentFileV1;
    use rt_path::SafeRelPath;

    fn meta() -> TorrentMetaV1 {
        TorrentMetaV1 {
            info_hash: [1u8; 20],
            announce: Some("http://tracker.example.com/announce".into()),
            announce_list: Vec::new(),
            name: "sample.bin".into(),
            piece_length: 16_384,
            pieces: vec![[2u8; 20], [3u8; 20]],
            files: vec![TorrentFileV1 {
                index: 0,
                length: 20_000,
                path: SafeRelPath::from_name("sample.bin", false).unwrap(),
                offset: 0,
            }],
            private: false,
            raw: b"torrent".to_vec(),
        }
    }

    fn raw_single_file_torrent() -> Vec<u8> {
        let pieces = vec![7u8; 20];
        let mut info_pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (b"length", BValue::Int(1024)),
            (b"name", BValue::Bytes(b"restore.bin")),
            (b"piece length", BValue::Int(16_384)),
            (b"pieces", BValue::Bytes(&pieces)),
        ];
        info_pairs.sort_by(|a, b| a.0.cmp(b.0));
        let info = BValue::Dict(info_pairs);
        let mut pairs: Vec<(&[u8], BValue<'_>)> = vec![
            (
                b"announce",
                BValue::Bytes(b"http://tracker.example.com/announce"),
            ),
            (b"info", info),
        ];
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        encode(&BValue::Dict(pairs))
    }

    #[test]
    fn row_conversion_preserves_session_fields() {
        let meta = meta();
        let mut entry = TorrentEntry::new("01".repeat(20), meta.name.clone(), "/tmp/data".into());
        entry.transition(TorrentState::Downloading).unwrap();
        entry.stats.add_download(10);
        entry.stats.add_upload(5);

        let row = row_from_entry(&entry, &meta);
        assert_eq!(row.info_hash, entry.info_hash);
        assert_eq!(row.state, "downloading");
        assert_eq!(row.total_length, 20_000);
        assert_eq!(row.piece_count, 2);
        assert_eq!(row.trackers, vec!["http://tracker.example.com/announce"]);

        let restored = entry_from_row(&row);
        assert_eq!(restored.info_hash, entry.info_hash);
        assert_eq!(restored.state, TorrentState::Downloading);
        assert_eq!(restored.stats.downloaded, 10);
        assert_eq!(restored.stats.uploaded, 5);
    }

    #[test]
    fn prune_empty_dirs_stops_at_root_and_keeps_nonempty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("downloads");
        let empty_leaf = root.join("torrent").join("subdir");
        std::fs::create_dir_all(&empty_leaf).unwrap();
        prune_empty_dirs(Some(&empty_leaf), &root).unwrap();
        assert!(root.exists());
        assert!(!root.join("torrent").exists());

        let nonempty = root.join("other").join("nested");
        std::fs::create_dir_all(&nonempty).unwrap();
        std::fs::write(root.join("other").join("keep.bin"), b"data").unwrap();
        prune_empty_dirs(Some(&nonempty), &root).unwrap();
        assert!(!nonempty.exists());
        assert!(root.join("other").exists());
        assert!(root.join("other").join("keep.bin").exists());
    }

    #[test]
    fn parse_info_hash_hex_rejects_invalid_input() {
        assert_eq!(parse_info_hash_hex(&"0a".repeat(20)).unwrap(), [10u8; 20]);
        assert!(parse_info_hash_hex("abc").is_err());
        assert!(parse_info_hash_hex(&"zz".repeat(20)).is_err());
    }

    #[test]
    fn metadata_projection_preserves_files_trackers_and_privacy() {
        let mut meta = meta();
        meta.private = true;
        meta.announce_list = vec![vec![
            "http://tracker.example.com/announce".into(),
            "udp://tracker.two:6969/announce".into(),
        ]];
        meta.files = vec![TorrentFileV1 {
            index: 7,
            length: 42,
            path: SafeRelPath::from_components(&["dir", "file.bin"], false).unwrap(),
            offset: 0,
        }];

        let projected = metadata_from_v1(&meta);

        assert_eq!(projected.piece_length, 16_384);
        assert_eq!(projected.piece_count, 2);
        assert_eq!(
            projected.piece_hashes,
            vec![hex::encode([2u8; 20]), hex::encode([3u8; 20])]
        );
        assert!(projected.is_private);
        assert_eq!(
            projected.trackers,
            vec![
                "http://tracker.example.com/announce".to_owned(),
                "udp://tracker.two:6969/announce".to_owned()
            ]
        );
        assert_eq!(projected.files.len(), 1);
        assert_eq!(projected.files[0].index, 7);
        assert_eq!(projected.files[0].path, "dir/file.bin");
        assert_eq!(projected.files[0].length, 42);
    }

    #[test]
    fn metadata_placeholder_projection_preserves_trackers() {
        let mut row = row_from_entry(
            &TorrentEntry::new("02".repeat(20), "pending".into(), "/tmp/data".into()),
            &meta(),
        );
        row.total_length = 0;
        row.piece_length = 0;
        row.piece_count = 0;
        row.trackers = vec!["udp://tracker.example:6969/announce".into()];
        row.state = "metadata_pending".into();

        let projected = metadata_from_placeholder_row(&row);

        assert_eq!(projected.piece_length, 0);
        assert_eq!(projected.piece_count, 0);
        assert!(projected.piece_hashes.is_empty());
        assert_eq!(projected.trackers, row.trackers);
        assert!(projected.files.is_empty());
    }

    #[test]
    fn label_normalization_trims_dedupes_and_drops_empty_values() {
        assert_eq!(
            normalize_category(Some(" movies ".into())).as_deref(),
            Some("movies")
        );
        assert_eq!(normalize_category(Some("   ".into())), None);
        assert_eq!(
            normalize_tags(vec![
                " hd ".into(),
                String::new(),
                "hd".into(),
                "archive".into()
            ]),
            vec!["hd".to_owned(), "archive".to_owned()]
        );
    }

    #[test]
    fn append_session_event_persists_payload() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };

        engine.append_session_event(
            Some(&"a".repeat(40)),
            EVENT_TORRENT_ADDED,
            Some("torrent added"),
            serde_json::json!({"paused": false}),
        );

        let db = engine.db.lock().unwrap();
        let events = rt_db::list_session_events(&db, Some(&"a".repeat(40)), 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EVENT_TORRENT_ADDED);
        assert_eq!(events[0].message.as_deref(), Some("torrent added"));
        assert!(events[0].payload.contains("\"paused\":false"));
    }

    #[test]
    fn recheck_job_helpers_persist_state_and_events() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };

        let job_id = engine.create_recheck_job(&"b".repeat(40)).unwrap();
        engine.update_job_state(&job_id, JOB_STATE_RUNNING, None, Some("recheck dispatched"));

        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.kind, JOB_KIND_RECHECK);
        assert_eq!(job.state, JOB_STATE_RUNNING);
        assert_eq!(job.affected_torrents, vec!["b".repeat(40)]);
        assert!(job.started_at.is_some());
        let events = rt_db::list_job_events(&db, &job_id, 10).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "check_started");
        assert_eq!(events[1].kind, "job_running");
        assert_eq!(events[2].kind, "job_queued");
    }

    #[tokio::test]
    async fn recheck_job_control_sends_torrent_commands_and_updates_state() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(4);
        let info_hash = "c".repeat(40);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };

        let job_id = engine.create_recheck_job(&info_hash).unwrap();
        engine
            .control_recheck_job(&job_id, JOB_STATE_PAUSED)
            .await
            .unwrap();
        assert!(matches!(torrent_rx.recv().await, Some(TorrentCmd::Pause)));

        engine
            .control_recheck_job(&job_id, JOB_STATE_RUNNING)
            .await
            .unwrap();
        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::Recheck { job_id: Some(id) }) if id == job_id
        ));

        engine
            .control_recheck_job(&job_id, JOB_STATE_CANCELLED)
            .await
            .unwrap();
        assert!(matches!(
            torrent_rx.recv().await,
            Some(TorrentCmd::CancelJob { job_id: id }) if id == job_id
        ));
        let db = engine.db.lock().unwrap();
        let job = rt_db::get_job(&db, &job_id).unwrap();
        assert_eq!(job.state, JOB_STATE_CANCELLED);
        assert!(job.finished_at.is_some());
    }

    #[tokio::test]
    async fn update_torrent_trackers_persists_summary_and_detail_rows() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let info_hash = "f".repeat(40);
        let mut entry = TorrentEntry::new(info_hash.clone(), "tracked".into(), "/data".into());
        entry.total_length = 1_000;
        entry.stats.downloaded = 250;
        let row = row_from_entry(&entry, &meta());
        rt_db::upsert(&conn, &row).unwrap();

        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        registry.write().await.add(entry).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };

        engine
            .update_torrent_trackers_inner(
                &info_hash,
                vec![
                    " udp://tracker.one/announce ".into(),
                    "udp://tracker.one/announce".into(),
                    "https://tracker.two/announce".into(),
                ],
            )
            .await
            .unwrap();

        let db = engine.db.lock().unwrap();
        let row = rt_db::get(&db, &info_hash).unwrap();
        assert_eq!(
            row.trackers,
            vec![
                "udp://tracker.one/announce".to_owned(),
                "https://tracker.two/announce".to_owned()
            ]
        );
        let trackers = rt_db::list_torrent_trackers(&db, &info_hash).unwrap();
        assert_eq!(trackers.len(), 2);
        assert_eq!(trackers[0].status, "pending");
        assert_eq!(trackers[0].left_bytes, 19_750);
    }

    #[tokio::test]
    async fn shutdown_torrent_tasks_sends_shutdown_and_waits_for_task_exit() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let (torrent_tx, mut torrent_rx) = mpsc::channel(1);
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let info_hash = "f".repeat(40);
        let mut torrent_chans = HashMap::new();
        torrent_chans.insert(info_hash.clone(), torrent_tx);
        let mut torrent_tasks = HashMap::new();
        torrent_tasks.insert(
            info_hash,
            tokio::spawn(async move {
                if matches!(torrent_rx.recv().await, Some(TorrentCmd::Shutdown)) {
                    let _ = seen_tx.send(());
                }
            }),
        );
        let mut engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans,
            torrent_tasks,
            dht_tx: None,
        };

        engine.shutdown_torrent_tasks().await;

        assert!(seen_rx.await.is_ok());
        assert!(engine.torrent_chans.is_empty());
        assert!(engine.torrent_tasks.is_empty());
    }

    #[test]
    fn recover_interrupted_jobs_pauses_running_work() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };
        let mut job = rt_db::JobRow {
            job_id: "job-running".to_owned(),
            kind: JOB_KIND_RECHECK.to_owned(),
            state: JOB_STATE_RUNNING.to_owned(),
            dry_run: false,
            affected_torrents: vec!["d".repeat(40)],
            total: 10,
            done: 4,
            checkpoint: 4,
            file_index: Some(0),
            piece_index: Some(4),
            byte_offset: Some(4096),
            verified_bytes: 4096,
            invalid_pieces: Vec::new(),
            error: None,
            created_at: 1,
            started_at: Some(2),
            updated_at: 3,
            finished_at: None,
        };
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert_job(&db, &job).unwrap();
        }
        job.job_id = "job-queued".to_owned();
        job.state = JOB_STATE_QUEUED.to_owned();
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert_job(&db, &job).unwrap();
        }

        engine.recover_interrupted_jobs().unwrap();

        let db = engine.db.lock().unwrap();
        let recovered = rt_db::get_job(&db, "job-running").unwrap();
        assert_eq!(recovered.state, JOB_STATE_PAUSED);
        assert_eq!(recovered.done, 4);
        assert_eq!(recovered.checkpoint, 4);
        assert_eq!(recovered.file_index, Some(0));
        assert_eq!(recovered.piece_index, Some(4));
        assert_eq!(recovered.byte_offset, Some(4096));
        assert_eq!(recovered.verified_bytes, 4096);
        assert!(recovered.finished_at.is_none());
        let queued = rt_db::get_job(&db, "job-queued").unwrap();
        assert_eq!(queued.state, JOB_STATE_QUEUED);
        let events = rt_db::list_job_events(&db, "job-running", 10).unwrap();
        assert_eq!(events[0].kind, "job_recovered");
    }

    #[tokio::test]
    async fn load_persisted_torrents_restores_registry_and_task_channels() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.daemon.session_dir = temp.path().join("session");
        config.db.path = temp.path().join("state.db");
        std::fs::create_dir_all(torrent_blob_dir(&config)).unwrap();
        std::fs::create_dir_all(fastresume_dir(&config)).unwrap();

        let conn = Connection::open(config.db_path()).unwrap();
        rt_db::migrate(&conn).unwrap();
        let raw = raw_single_file_torrent();
        let TorrentMeta::V1(meta) = parse_torrent(&raw).unwrap() else {
            panic!("expected v1 torrent");
        };
        let info_hash = meta
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        std::fs::write(torrent_blob_path(&config, &info_hash), &raw).unwrap();
        rt_db::upsert(
            &conn,
            &TorrentRow {
                info_hash: info_hash.clone(),
                name: meta.name.clone(),
                total_length: meta.total_length() as i64,
                piece_length: meta.piece_length as i64,
                piece_count: meta.pieces.len() as i64,
                is_private: false,
                save_path: temp.path().join("downloads").to_string_lossy().to_string(),
                category: Some("movies".to_owned()),
                tags: vec!["restored".to_owned()],
                state: "paused".to_owned(),
                added_at: 10,
                completed_at: None,
                uploaded: 5,
                downloaded: 7,
                ratio: 0.0,
                trackers: meta.all_trackers(),
            },
        )
        .unwrap();

        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let mut engine = Engine {
            config: Arc::new(config),
            registry: Arc::clone(&registry),
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };

        engine.load_persisted_torrents().await.unwrap();

        assert!(engine.torrent_chans.contains_key(&info_hash));
        let reg = registry.read().await;
        let restored = reg.get(&info_hash).unwrap();
        assert_eq!(restored.state, TorrentState::Paused);
        assert_eq!(restored.category.as_deref(), Some("movies"));
        assert_eq!(restored.tags, vec!["restored".to_owned()]);
        drop(reg);
        if let Some(tx) = engine.torrent_chans.remove(&info_hash) {
            let _ = tx.send(TorrentCmd::Shutdown).await;
        }
    }

    #[test]
    fn register_configured_storage_persists_root_and_mount() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.storage.download_dir = temp.path().join("downloads");
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();

        register_configured_storage(&conn, &config).unwrap();

        let roots = rt_db::list_storage_roots(&conn).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].profile, "auto");
        assert!(roots[0].path.ends_with("downloads"));
        let mounts = rt_db::list_mounts(&conn).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].queue_depth, 1);
        assert_eq!(mounts[0].read_concurrency, 1);
        assert_eq!(mounts[0].write_concurrency, 1);
    }

    #[tokio::test]
    async fn engine_stats_include_registry_jobs_and_trackers() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("e".repeat(40), "stats.bin".into(), "/tmp".into());
            entry.stats.add_upload(10);
            entry.stats.add_download(20);
            entry.amount_left = 30;
            let _ = entry.transition(TorrentState::Downloading);
            reg.add(entry).unwrap();
        }
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };
        let job_id = engine.create_recheck_job(&"e".repeat(40)).unwrap();
        engine.update_job_state(&job_id, JOB_STATE_RUNNING, None, Some("running"));
        {
            let mut db = engine.db.lock().unwrap();
            rt_db::upsert(
                &db,
                &TorrentRow {
                    info_hash: "e".repeat(40),
                    name: "stats.bin".to_owned(),
                    total_length: 100,
                    piece_length: 10,
                    piece_count: 10,
                    is_private: false,
                    save_path: "/tmp".to_owned(),
                    category: None,
                    tags: Vec::new(),
                    state: "downloading".to_owned(),
                    added_at: 1,
                    completed_at: None,
                    uploaded: 10,
                    downloaded: 20,
                    ratio: 0.5,
                    trackers: Vec::new(),
                },
            )
            .unwrap();
            rt_db::replace_torrent_trackers(
                &mut db,
                &"e".repeat(40),
                &[rt_db::TorrentTrackerRow {
                    info_hash: "e".repeat(40),
                    tracker_index: 0,
                    tier: 0,
                    url: "http://tracker/announce".to_owned(),
                    status: "error".to_owned(),
                    last_announce_at: None,
                    next_announce_at: None,
                    last_success_at: None,
                    failure_reason: Some("timeout".to_owned()),
                    warning_message: None,
                    seeders: None,
                    leechers: None,
                    completed: None,
                    uploaded: 10,
                    downloaded: 20,
                    left_bytes: 30,
                }],
            )
            .unwrap();
        }

        let stats = engine.engine_stats().await.unwrap();
        assert_eq!(stats.torrents_total, 1);
        assert_eq!(stats.torrents_downloading, 1);
        assert_eq!(stats.bytes_uploaded, 10);
        assert_eq!(stats.bytes_downloaded, 20);
        assert_eq!(stats.bytes_left, 30);
        assert_eq!(stats.jobs_active, 1);
        assert_eq!(stats.trackers_total, 1);
        assert_eq!(stats.trackers_error, 1);
    }

    #[tokio::test]
    async fn torrent_diagnostic_explains_paused_private_tracker_gap() {
        let conn = Connection::open_in_memory().unwrap();
        rt_db::migrate(&conn).unwrap();
        let (_tx, rx) = mpsc::channel(1);
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let info_hash = "f".repeat(40);
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new(info_hash.clone(), "why.bin".into(), "/tmp".into());
            entry.amount_left = 100;
            let _ = entry.transition(TorrentState::Paused);
            reg.add(entry).unwrap();
        }
        let engine = Engine {
            config: Arc::new(Config::default()),
            registry,
            db: Arc::new(Mutex::new(conn)),
            cmd_rx: rx,
            cmd_tx: mpsc::channel(1).0,
            torrent_chans: HashMap::new(),
            torrent_tasks: HashMap::new(),
            dht_tx: None,
        };
        {
            let db = engine.db.lock().unwrap();
            rt_db::upsert(
                &db,
                &TorrentRow {
                    info_hash: info_hash.clone(),
                    name: "why.bin".to_owned(),
                    total_length: 100,
                    piece_length: 10,
                    piece_count: 10,
                    is_private: true,
                    save_path: "/tmp".to_owned(),
                    category: None,
                    tags: Vec::new(),
                    state: "paused".to_owned(),
                    added_at: 1,
                    completed_at: None,
                    uploaded: 0,
                    downloaded: 0,
                    ratio: 0.0,
                    trackers: Vec::new(),
                },
            )
            .unwrap();
        }

        let diagnostic = engine.diagnose_torrent_inner(&info_hash).await.unwrap();
        assert_eq!(diagnostic.state, "paused");
        assert!(diagnostic
            .reasons
            .iter()
            .any(|reason| reason.contains("paused")));
        assert!(diagnostic
            .reasons
            .iter()
            .any(|reason| reason.contains("no persisted trackers")));
    }
}

/// Handle an incoming TCP peer connection: read the handshake's info_hash and
/// forward the stream to the matching torrent task.
async fn handle_incoming(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    torrent_chans: HashMap<String, mpsc::Sender<TorrentCmd>>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut hs = [0u8; HANDSHAKE_LEN];
    stream.read_exact(&mut hs).await?;
    let handshake = Handshake::parse(&hs)?;
    let info_hash_hex: String = handshake
        .info_hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let tx = torrent_chans
        .get(&info_hash_hex)
        .ok_or_else(|| anyhow::anyhow!("no torrent for incoming info_hash {info_hash_hex}"))?;
    tx.send(TorrentCmd::AcceptPeer {
        stream,
        peer_addr,
        handshake,
    })
    .await
    .map_err(|_| anyhow::anyhow!("torrent task gone for incoming info_hash {info_hash_hex}"))?;
    Ok(())
}
