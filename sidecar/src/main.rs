use rtorrentng::{api, cache, config, metrics, qbcompat as _, rtorrent, sync};
use std::sync::Arc;
use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tracing::info;

use api::{server::{AppState, build_router}, ws::Event};
use cache::Db;
use config::Config;
use metrics::Metrics;
use rtorrent::Client;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg_path = std::env::args().nth(1);
    let cfg = Config::load(cfg_path.as_deref()).context("load config")?;

    let filter = if cfg.debug { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| filter.into())
        )
        .init();

    info!("rtorrentNG starting");
    info!(user_agent = %cfg.rtorrent.user_agent, "config loaded");

    let rt = Arc::new(Client::new(&cfg.rtorrent).context("create rtorrent client")?);

    if let Err(e) = rt.set_user_agent(&cfg.rtorrent.user_agent).await {
        tracing::warn!("could not set user agent on startup (rTorrent may not be ready yet): {e}");
    }

    let db = Arc::new(Db::open(&cfg.cache_path()).context("open cache db")?);
    let metrics = Metrics::new();
    let (tx, _) = broadcast::channel::<Event>(1024);

    {
        let rt2 = rt.clone();
        let db2 = db.clone();
        let tx2 = tx.clone();
        let mx2 = metrics.clone();
        let interval = cfg.sync_interval();
        tokio::spawn(async move {
            sync::run(rt2, db2, tx2, mx2, interval).await;
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

    let addr: std::net::SocketAddr = cfg.listen_addr.parse()
        .with_context(|| format!("parse listen_addr {}", cfg.listen_addr))?;

    info!(%addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await
        .with_context(|| format!("bind {addr}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("http server")?;

    info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.expect("ctrl-c listener"); };

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
