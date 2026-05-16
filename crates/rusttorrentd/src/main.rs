use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;
use tracing::info;

use rt_api_native::state::AppState as NativeState;
use rt_api_qbit::state::AppState as QbitState;
use rt_config::Config;
use rt_engine::Engine;
use rt_session::SessionRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging — JSON in production, pretty in dev
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();

    let config = Arc::new(load_config());
    info!(
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %config.daemon.api_bind,
        listen_port = config.network.listen_port,
        "rusttorrentd starting"
    );

    // Ensure session directory exists
    std::fs::create_dir_all(&config.daemon.session_dir)
        .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;

    // Shared in-memory session registry
    let registry = Arc::new(RwLock::new(SessionRegistry::new()));

    // Start the engine (TCP listener + torrent task supervisor)
    let engine_handle = Engine::start(Arc::clone(&config), Arc::clone(&registry))
        .await
        .context("starting engine")?;

    // Build the API routers
    let native_state = NativeState::with_engine(Arc::clone(&registry), engine_handle.clone());
    let native_router = rt_api_native::router::build_router(native_state);

    let qbit_state = QbitState::with_engine(Arc::clone(&registry), engine_handle.clone());
    let qbit_router = rt_api_qbit::router::build_qbit_router(qbit_state);

    // Merge into a single axum app
    let app = native_router.merge(qbit_router);

    let api_addr: std::net::SocketAddr = config
        .daemon
        .api_bind
        .parse()
        .context("invalid api_bind address")?;

    info!(addr = %api_addr, "API listening");

    let listener = tokio::net::TcpListener::bind(api_addr)
        .await
        .with_context(|| format!("binding API to {api_addr}"))?;

    // Graceful shutdown on ctrl-c
    let engine_for_shutdown = engine_handle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("received ctrl-c, shutting down");
            engine_for_shutdown.shutdown().await;
        }
    });

    axum::serve(listener, app.into_make_service())
        .await
        .context("API server error")?;

    Ok(())
}

fn load_config() -> Config {
    // Allow explicit path via env var
    if let Ok(path) = std::env::var("RUSTTORRENTD_CONFIG") {
        match Config::load(std::path::Path::new(&path)) {
            Ok(c) => return c,
            Err(e) => eprintln!("config error: {e}"),
        }
    }
    Config::load_default()
}
