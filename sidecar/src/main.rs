use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::broadcast,
    task::{JoinError, JoinHandle},
};
use torrentng::{api, backend, cache, config, metrics, rtorrent, rtorrent_logs, stats, sync};
use tracing::{info, warn};

use api::{
    server::{build_router, AppState},
    ws::Event,
};
use backend::TorrentBackend;
use cache::{AppEventRow, Db};
use config::{BackendKind, Config};
use metrics::Metrics;
use rtorrent::Client;

struct BackgroundTasks {
    sync: JoinHandle<()>,
    stats: JoinHandle<()>,
    rtorrent_logs: Option<JoinHandle<()>>,
}

impl BackgroundTasks {
    async fn wait_for_exit(&mut self) -> anyhow::Error {
        tokio::select! {
            biased;
            result = &mut self.sync => task_exit_error("sync loop", result),
            result = &mut self.stats => task_exit_error("stats loop", result),
            result = wait_for_optional_task(&mut self.rtorrent_logs) => {
                match result {
                    Some(result) => task_exit_error("rTorrent log ingestion loop", result),
                    None => unreachable!("disabled optional task has a pending waiter"),
                }
            }
        }
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        self.sync.abort();
        self.stats.abort();
        if let Some(task) = &self.rtorrent_logs {
            task.abort();
        }
    }
}

async fn wait_for_optional_task(
    task: &mut Option<JoinHandle<()>>,
) -> Option<std::result::Result<(), JoinError>> {
    match task {
        Some(task) => Some(task.await),
        None => std::future::pending().await,
    }
}

fn task_exit_error(
    task: &'static str,
    result: std::result::Result<(), JoinError>,
) -> anyhow::Error {
    match result {
        Ok(()) => anyhow::anyhow!("{task} exited unexpectedly"),
        Err(error) => {
            let outcome = if error.is_panic() {
                "panicked"
            } else {
                "was cancelled"
            };
            anyhow::anyhow!("{task} {outcome}")
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg_path = std::env::args().nth(1);
    let cfg = Config::load(cfg_path.as_deref()).context("load config")?;

    let legacy_filter = if cfg.debug { "debug" } else { "info" };
    rt_logging::init(&cfg.logging, Some(legacy_filter));

    info!(
        component = "sidecar",
        operation = "startup",
        version = env!("CARGO_PKG_VERSION"),
        result = "started",
        "TorrentNG compatible-client service starting"
    );
    info!(
        component = "config",
        operation = "load",
        user_agent_len = cfg.rtorrent.user_agent.len(),
        log_format = ?cfg.logging.format,
        log_profile = ?cfg.logging.profile,
        event_retention = cfg.logging.event_retention,
        rtorrent_log_ingest = cfg.rtorrent.logs.enabled,
        result = "ok",
        "config loaded"
    );

    let rt = Arc::new(Client::new(&cfg.rtorrent).context("create rtorrent client")?);
    let backend: Arc<dyn TorrentBackend> = match cfg.backend.backend_type {
        BackendKind::Rtorrent => Arc::new(backend::rtorrent::RtorrentBackend::new(rt.clone())),
        BackendKind::Qbittorrent => Arc::new(
            backend::qbittorrent::QbittorrentBackend::new(&cfg.qbittorrent)
                .context("create qbittorrent backend")?,
        ),
        BackendKind::Transmission => Arc::new(
            backend::transmission::TransmissionBackend::new(&cfg.transmission)
                .context("create transmission backend")?,
        ),
        BackendKind::Deluge => Arc::new(
            backend::deluge::DelugeBackend::new(&cfg.deluge).context("create deluge backend")?,
        ),
        BackendKind::Torrentng => Arc::new(
            backend::torrentng::TorrentngBackend::new(&cfg.torrentng)
                .context("create TorrentNG client backend")?,
        ),
    };

    let db = Arc::new(Db::open(&cfg.cache_path()).context("open cache db")?);
    append_startup_event(
        &db,
        cfg.logging.event_retention,
        "info",
        "sidecar_started",
        "TorrentNG compatible-client service started",
        serde_json::json!({
            "component": "sidecar",
            "operation": "startup",
        }),
    );
    let metrics = Metrics::new();
    let (tx, _) = broadcast::channel::<Event>(1024);

    if cfg.backend.backend_type == BackendKind::Rtorrent {
        if let Err(error) =
            initialize_rtorrent_identity(&rt, &cfg.rtorrent.user_agent, &cfg.rtorrent.peer_id).await
        {
            warn!(
                component = "rtorrent",
                operation = "startup_identity",
                result = "error",
                error = %torrentng::redact_log_display(&error),
                "refusing to serve until rTorrent tracker identity is applied"
            );
            append_startup_event(
                &db,
                cfg.logging.event_retention,
                "error",
                "rtorrent_identity_error",
                "compatible-client service refused startup because rTorrent tracker identity was not applied",
                serde_json::json!({
                    "component": "rtorrent",
                    "operation": "startup_identity",
                    "result": "error",
                    "error": error.to_string(),
                }),
            );
            return Err(error).context("initialize rTorrent tracker identity");
        }
        append_startup_event(
            &db,
            cfg.logging.event_retention,
            "info",
            "rtorrent_identity_ready",
            "rTorrent tracker identity applied before compatible-client service became available",
            serde_json::json!({
                "component": "rtorrent",
                "operation": "startup_identity",
                "result": "ok",
                "peer_id_length": cfg.rtorrent.peer_id.len(),
            }),
        );
    }

    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        rt,
        backend: backend.clone(),
        db: db.clone(),
        events: tx.clone(),
        metrics: metrics.clone(),
        qbit_search_plugins: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_search_jobs: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_next_search_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        login_attempt_limiter: torrentng::auth::LoginAttemptLimiter::default(),
        control_plane_write: Arc::new(tokio::sync::Mutex::new(())),
    };
    let app = build_router(state);

    let addr: std::net::SocketAddr = cfg
        .listen_addr
        .parse()
        .with_context(|| format!("parse listen_addr {}", cfg.listen_addr))?;

    info!(
        component = "http",
        operation = "listen",
        %addr,
        result = "started",
        "listening"
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    let sync_backend = backend.clone();
    let sync_db = db.clone();
    let sync_events = tx.clone();
    let sync_metrics = metrics.clone();
    let sync_interval = cfg.sync_interval();
    let retention = cfg.logging.event_retention;
    let sync_task = tokio::spawn(async move {
        sync::run(
            sync_backend,
            sync_db,
            sync_events,
            sync_metrics,
            sync_interval,
            retention,
        )
        .await;
    });

    let stats_backend = backend.clone();
    let stats_db = db.clone();
    let stats_events = tx.clone();
    let retention = cfg.logging.event_retention;
    let stats_task = tokio::spawn(async move {
        stats::run(
            stats_backend,
            stats_db,
            stats_events,
            std::time::Duration::from_secs(2),
            retention,
        )
        .await;
    });

    let rtorrent_logs_task = if cfg.backend.backend_type == BackendKind::Rtorrent
        && cfg.rtorrent.logs.enabled
        && !cfg.rtorrent.logs.paths.is_empty()
    {
        let log_db = db.clone();
        let log_cfg = cfg.rtorrent.logs.clone();
        let retention = cfg.logging.event_retention;
        Some(tokio::spawn(async move {
            rtorrent_logs::run(log_db, log_cfg, retention).await;
        }))
    } else {
        None
    };
    let mut background_tasks = BackgroundTasks {
        sync: sync_task,
        stats: stats_task,
        rtorrent_logs: rtorrent_logs_task,
    };

    let server = async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
    };
    tokio::pin!(server);
    tokio::select! {
        biased;
        error = background_tasks.wait_for_exit() => {
            tracing::error!(
                component = "sidecar",
                operation = "background_task",
                result = "stopped",
                error = %error,
                "sidecar background task exited; terminating service"
            );
            return Err(error);
        }
        result = &mut server => result.context("http server")?,
    }

    info!(
        component = "sidecar",
        operation = "shutdown",
        result = "ok",
        "shutdown complete"
    );
    Ok(())
}

/// Apply every tracker-facing rTorrent identity before starting any
/// compatible-client service
/// background task or binding the HTTP listener. The packaged rTorrent build
/// keeps session torrents behind its identity gate until release_identity_gate
/// succeeds, so startup cannot expose a window where a torrent announces with
/// a stale or default local_id.
async fn initialize_rtorrent_identity(rt: &Client, user_agent: &str, peer_id: &str) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=3 {
        let result = async {
            // Use the XML-RPC path for all mutators. The JSON adapter can
            // acknowledge some rTorrent state-changing calls without applying
            // the underlying transition.
            rt.set_user_agent(user_agent).await?;
            rt.set_all_peer_ids(peer_id).await?;
            rt.release_identity_gate().await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => {
                tracing::info!(
                    component = "rtorrent",
                    operation = "startup_identity",
                    result = "ok",
                    attempt,
                    "rTorrent tracker identity is ready"
                );
                return Ok(());
            }
            Err(error) => {
                warn!(
                    component = "rtorrent",
                    operation = "startup_identity",
                    result = "retry",
                    attempt,
                    error = %torrentng::redact_log_display(&error),
                    "rTorrent tracker identity setup failed"
                );
                last_error = Some(error);
                if attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    Err(last_error.expect("identity setup always attempts at least once"))
}

fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn append_startup_event(
    db: &Db,
    retention: usize,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
) {
    if let Err(e) = db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: unix_now_i64(),
            level: level.to_owned(),
            kind: kind.to_owned(),
            message: message.to_owned(),
            payload: payload.to_string(),
        },
        retention,
    ) {
        warn!(
            component = "app_events",
            operation = "append",
            kind,
            result = "error",
            error = %torrentng::redact_log_display(&e),
            "failed to append startup app event"
        );
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("ctrl-c listener");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn supervised_background_panic_is_reported_without_its_payload() {
        let sync = tokio::spawn(async {
            std::panic::panic_any("sidecar-background-panic-canary");
        });
        let mut tasks = BackgroundTasks {
            sync,
            stats: tokio::spawn(std::future::pending()),
            rtorrent_logs: None,
        };

        let error = tasks.wait_for_exit().await;
        let rendered = format!("{error:#}");
        assert_eq!(rendered, "sync loop panicked");
        assert!(!rendered.contains("sidecar-background-panic-canary"));
    }

    #[tokio::test]
    async fn supervised_background_cancellation_is_not_silent() {
        let sync = tokio::spawn(std::future::pending());
        sync.abort();
        let mut tasks = BackgroundTasks {
            sync,
            stats: tokio::spawn(std::future::pending()),
            rtorrent_logs: None,
        };

        assert_eq!(
            tasks.wait_for_exit().await.to_string(),
            "sync loop was cancelled"
        );
    }

    #[tokio::test]
    async fn normally_returned_background_loop_is_still_unexpected() {
        let result = tokio::spawn(async {}).await;
        assert_eq!(
            task_exit_error("stats loop", result).to_string(),
            "stats loop exited unexpectedly"
        );
    }
}
