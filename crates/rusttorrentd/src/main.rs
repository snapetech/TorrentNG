use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::Context;
use axum::{
    body::Body,
    extract::MatchedPath,
    http::{header, Request},
    middleware::{self, Next},
    response::Response,
};
use tokio::sync::RwLock;
use tracing::info;

use rt_api_deluge::AppState as DelugeState;
use rt_api_native::state::AppState as NativeState;
use rt_api_qbit::state::AppState as QbitState;
use rt_api_transmission::AppState as TransmissionState;
use rt_config::Config;
use rt_engine::Engine;
use rt_session::SessionRegistry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Arc::new(load_config());
    rt_logging::init(&config.logging, Some(&config.daemon.log_level));
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
    let native_state = NativeState::with_engine_and_tokens(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
    );
    let native_router = rt_api_native::router::build_router(native_state);

    let qbit_state = QbitState::with_engine(Arc::clone(&registry), engine_handle.clone());
    let qbit_router = rt_api_qbit::router::build_qbit_router(qbit_state);

    let transmission_state =
        TransmissionState::with_engine(Arc::clone(&registry), engine_handle.clone());
    let transmission_router = rt_api_transmission::build_transmission_router(transmission_state);

    let deluge_state = DelugeState::with_engine(Arc::clone(&registry), engine_handle.clone());
    let deluge_router = rt_api_deluge::build_deluge_router(deluge_state);

    // Merge into a single axum app
    let app = native_router
        .merge(qbit_router)
        .merge(transmission_router)
        .merge(deluge_router)
        .layer(middleware::from_fn(request_log));

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

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

async fn request_log(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    if skip_request_log(&path) {
        return next.run(req).await;
    }
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    let duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let response_size = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    tracing::info!(
        component = "http",
        operation = "request",
        request_id,
        method = %method,
        path = %path,
        route = route.as_deref(),
        status = status.as_u16(),
        duration_ms,
        response_size,
        result = if status.is_server_error() { "error" } else { "ok" },
        "http request completed"
    );
    response
}

fn skip_request_log(path: &str) -> bool {
    path == "/health"
        || path == "/metrics"
        || path == "/ws"
        || path == "/favicon.ico"
        || path.starts_with("/assets/")
        || path.starts_with("/static/")
        || is_static_asset_path(path)
}

fn is_static_asset_path(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("css" | "js" | "map" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico")
    )
}

#[cfg(test)]
mod tests {
    use super::skip_request_log;

    #[test]
    fn request_log_skips_health_metrics_ws_and_static_assets() {
        for path in [
            "/health",
            "/metrics",
            "/ws",
            "/favicon.ico",
            "/assets/app.js",
            "/static/theme.css",
            "/index.css",
            "/logo.svg",
        ] {
            assert!(skip_request_log(path), "{path}");
        }
        assert!(!skip_request_log("/api/v1/torrents"));
        assert!(!skip_request_log("/api/qb/v2/log/main"));
    }
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
