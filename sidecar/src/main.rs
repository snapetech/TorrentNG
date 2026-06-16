use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;
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
        "TorrentNG sidecar starting"
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
                .context("create torrentng native backend")?,
        ),
    };

    let db = Arc::new(Db::open(&cfg.cache_path()).context("open cache db")?);
    append_startup_event(
        &db,
        cfg.logging.event_retention,
        "info",
        "sidecar_started",
        "TorrentNG sidecar started",
        serde_json::json!({
            "component": "sidecar",
            "operation": "startup",
        }),
    );
    let metrics = Metrics::new();
    let (tx, _) = broadcast::channel::<Event>(1024);

    if cfg.backend.backend_type == BackendKind::Rtorrent {
        let rt_ua = rt.clone();
        let ua = cfg.rtorrent.user_agent.clone();
        let peer_id = cfg.rtorrent.peer_id.clone();
        let db2 = db.clone();
        let retention = cfg.logging.event_retention;
        tokio::spawn(async move {
            let mut user_agent_error = None;
            for attempt in 1..=3 {
                match rt_ua.set_user_agent(&ua).await {
                    Ok(()) => {
                        user_agent_error = None;
                        break;
                    }
                    Err(e) => {
                        user_agent_error = Some(e);
                        if attempt < 3 {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                }
            }
            if let Some(e) = user_agent_error {
                warn!(
                    component = "rtorrent",
                    operation = "set_user_agent",
                    result = "error",
                    error = %e,
                    "could not set user agent after startup"
                );
                append_startup_event(
                    &db2,
                    retention,
                    "warn",
                    "rtorrent_user_agent_error",
                    "could not apply rTorrent user agent after startup",
                    serde_json::json!({
                        "component": "rtorrent",
                        "operation": "set_user_agent",
                        "result": "error",
                        "error": e.to_string(),
                    }),
                );
            }
            if let Err(e) = rt_ua.set_all_peer_ids(&peer_id).await {
                warn!(
                    component = "rtorrent",
                    operation = "set_peer_id",
                    result = "error",
                    error = %e,
                    "could not set rTorrent peer id after startup"
                );
                append_startup_event(
                    &db2,
                    retention,
                    "warn",
                    "rtorrent_peer_id_error",
                    "could not apply rTorrent peer id after startup",
                    serde_json::json!({
                        "component": "rtorrent",
                        "operation": "set_peer_id",
                        "result": "error",
                        "error": e.to_string(),
                    }),
                );
            }
        });
    }

    {
        let backend2 = backend.clone();
        let db2 = db.clone();
        let tx2 = tx.clone();
        let mx2 = metrics.clone();
        let interval = cfg.sync_interval();
        let retention = cfg.logging.event_retention;
        tokio::spawn(async move {
            sync::run(backend2, db2, tx2, mx2, interval, retention).await;
        });
    }

    {
        let backend2 = backend.clone();
        let db2 = db.clone();
        let tx2 = tx.clone();
        let retention = cfg.logging.event_retention;
        tokio::spawn(async move {
            stats::run(
                backend2,
                db2,
                tx2,
                std::time::Duration::from_secs(2),
                retention,
            )
            .await;
        });
    }

    if cfg.backend.backend_type == BackendKind::Rtorrent
        && cfg.rtorrent.logs.enabled
        && !cfg.rtorrent.logs.paths.is_empty()
    {
        let db2 = db.clone();
        let log_cfg = cfg.rtorrent.logs.clone();
        let retention = cfg.logging.event_retention;
        tokio::spawn(async move {
            rtorrent_logs::run(db2, log_cfg, retention).await;
        });
    }

    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        rt,
        backend,
        db,
        events: tx,
        metrics,
        qbit_search_plugins: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_search_jobs: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_next_search_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        qbit_rss_items: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
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

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("http server")?;

    info!(
        component = "sidecar",
        operation = "shutdown",
        result = "ok",
        "shutdown complete"
    );
    Ok(())
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
            error = %e,
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
