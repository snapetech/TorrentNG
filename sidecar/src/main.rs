use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::broadcast;
use torrentng::{api, cache, config, metrics, rtorrent, rtorrent_logs, stats, sync};
use tracing::info;

use api::{
    server::{build_router, AppState},
    ws::Event,
};
use cache::{AppEventRow, Db};
use config::Config;
use metrics::Metrics;
use rtorrent::Client;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg_path = std::env::args().nth(1);
    let cfg = Config::load(cfg_path.as_deref()).context("load config")?;

    let legacy_filter = if cfg.debug { "debug" } else { "info" };
    rt_logging::init(&cfg.logging, Some(legacy_filter));

    info!("TorrentNG starting");
    info!(user_agent = %cfg.rtorrent.user_agent, "config loaded");

    let rt = Arc::new(Client::new(&cfg.rtorrent).context("create rtorrent client")?);

    let db = Arc::new(Db::open(&cfg.cache_path()).context("open cache db")?);
    let _ = db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: unix_now_i64(),
            level: "info".to_owned(),
            kind: "sidecar_started".to_owned(),
            message: "TorrentNG sidecar started".to_owned(),
            payload: serde_json::json!({
                "component": "sidecar",
                "operation": "startup",
            })
            .to_string(),
        },
        cfg.logging.event_retention,
    );
    let metrics = Metrics::new();
    let (tx, _) = broadcast::channel::<Event>(1024);

    {
        let rt_ua = rt.clone();
        let ua = cfg.rtorrent.user_agent.clone();
        tokio::spawn(async move {
            if let Err(e) = rt_ua.set_user_agent(&ua).await {
                tracing::warn!(
                    component = "rtorrent",
                    operation = "set_user_agent",
                    error = %e,
                    "could not set user agent after startup"
                );
            }
        });
    }

    {
        let rt2 = rt.clone();
        let db2 = db.clone();
        let tx2 = tx.clone();
        let mx2 = metrics.clone();
        let interval = cfg.sync_interval();
        let retention = cfg.logging.event_retention;
        tokio::spawn(async move {
            sync::run(rt2, db2, tx2, mx2, interval, retention).await;
        });
    }

    {
        let rt2 = rt.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            stats::run(rt2, tx2, std::time::Duration::from_secs(2)).await;
        });
    }

    if cfg.rtorrent.logs.enabled && !cfg.rtorrent.logs.paths.is_empty() {
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
        db,
        events: tx,
        metrics,
    };
    let app = build_router(state);

    let addr: std::net::SocketAddr = cfg
        .listen_addr
        .parse()
        .with_context(|| format!("parse listen_addr {}", cfg.listen_addr))?;

    info!(%addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http server")?;

    info!("shutdown complete");
    Ok(())
}

fn unix_now_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
