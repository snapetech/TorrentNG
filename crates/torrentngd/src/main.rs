use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::Context;
use axum::{
    body::Body,
    extract::{ConnectInfo, MatchedPath, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tokio::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use rt_api_deluge::AppState as DelugeState;
use rt_api_model::ApiRuntimeMetrics;
use rt_api_native::state::AppState as NativeState;
use rt_api_qbit::state::AppState as QbitState;
use rt_api_transmission::AppState as TransmissionState;
use rt_config::Config;
use rt_engine::Engine;
use rt_session::SessionRegistry;

mod export;
mod migrate;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print_help();
            return Ok(());
        }
        Some("-V" | "--version" | "version") => {
            println!("torrentngd {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("migrate") => {
            let rest = argv[2..].to_vec();
            return tokio::task::spawn_blocking(move || migrate::run(&rest))
                .await
                .context("migrate task panicked")?;
        }
        Some("export") => {
            let rest = argv[2..].to_vec();
            return tokio::task::spawn_blocking(move || export::run(&rest))
                .await
                .context("export task panicked")?;
        }
        _ => {}
    }

    let config = Arc::new(load_config()?);
    rt_logging::init(&config.logging, Some(&config.daemon.log_level));
    info!(
        component = "daemon",
        operation = "startup",
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %config.daemon.api_bind,
        listen_port = config.network.listen_port,
        "torrentngd starting"
    );

    // Ensure session directory exists
    std::fs::create_dir_all(&config.daemon.session_dir)
        .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;

    // Resolve (and persist, if not already done) this install's tracker
    // peer id before any engine/tracker task can observe it. Must run
    // before Engine::start. See docs/TRACKER-IDENTITY.md.
    rt_engine::peer_id::init(&config.daemon.session_dir);

    // Shared in-memory session registry
    let registry = Arc::new(RwLock::new(SessionRegistry::new()));

    // Start the engine (TCP listener + torrent task supervisor)
    let engine_handle = Engine::start(Arc::clone(&config), Arc::clone(&registry))
        .await
        .context("starting engine")?;

    // Build the API routers
    let api_metrics = ApiRuntimeMetrics::new();
    let native_state = NativeState::with_engine_and_tokens_and_metrics(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
        Arc::clone(&api_metrics),
    );
    let native_router = rt_api_native::router::build_router(native_state);

    let qbit_state = QbitState::with_engine_and_tokens_and_metrics(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
        Arc::clone(&api_metrics),
    );
    let qbit_router = rt_api_qbit::router::build_qbit_router(qbit_state);

    let transmission_state = TransmissionState::with_engine_and_tokens(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
    );
    let transmission_router = rt_api_transmission::build_transmission_router(transmission_state);

    let deluge_state = DelugeState::with_engine_and_tokens(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
    );
    let deluge_router = rt_api_deluge::build_deluge_router(deluge_state);

    // Merge into a single axum app
    let static_dir = static_dir();
    let static_index = static_dir.join("index.html");
    if static_index.exists() {
        info!(
            component = "http",
            operation = "static_webui",
            static_dir = %static_dir.display(),
            "serving WebUI assets"
        );
    } else {
        tracing::warn!(
            component = "http",
            operation = "static_webui",
            static_dir = %static_dir.display(),
            "WebUI index.html not found; API will run but / will return 404"
        );
    }

    let app = native_router
        .merge(qbit_router)
        .merge(transmission_router)
        .merge(deluge_router)
        .fallback_service(
            ServeDir::new(&static_dir).not_found_service(ServeFile::new(&static_index)),
        )
        .layer(middleware::from_fn(request_log))
        .layer(middleware::from_fn_with_state(
            Arc::new(config.auth.api_tokens.clone()),
            daemon_auth_guard,
        ));

    let api_addr: std::net::SocketAddr = config
        .daemon
        .api_bind
        .parse()
        .context("invalid api_bind address")?;

    info!(
        component = "http",
        operation = "listen",
        addr = %api_addr,
        "API listening"
    );

    let listener = tokio::net::TcpListener::bind(api_addr)
        .await
        .with_context(|| format!("binding API to {api_addr}"))?;

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(engine_handle))
    .await
    .context("API server error")?;

    Ok(())
}

async fn shutdown_signal(engine: rt_engine::EngineHandle) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if result.is_ok() {
                    info!(
                        component = "daemon",
                        operation = "shutdown_signal",
                        signal = "ctrl-c",
                        "received shutdown signal"
                    );
                }
            }
            _ = async {
                if let Some(signal) = sigterm.as_mut() {
                    signal.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                info!(
                    component = "daemon",
                    operation = "shutdown_signal",
                    signal = "sigterm",
                    "received shutdown signal"
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!(
            component = "daemon",
            operation = "shutdown_signal",
            signal = "ctrl-c",
            "received shutdown signal"
        );
    }
    engine.shutdown().await;
}

fn print_help() {
    println!(
        "torrentngd {}\n\nUSAGE:\n    torrentngd [migrate|export] [OPTIONS]\n\nENV:\n    TORRENTNGD_CONFIG  Path to native daemon config\n    TNG_STATIC_DIR     Built WebUI directory to serve, default /usr/share/torrentng/webui\n\nCOMMANDS:\n    migrate            Import existing client state into the native engine\n    export             Export native state for another client\n    help               Print this help\n    version            Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn static_dir() -> PathBuf {
    std::env::var_os("TNG_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/share/torrentng/webui"))
}

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

async fn daemon_auth_guard(
    State(api_tokens): State<Arc<Vec<String>>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if api_tokens.is_empty()
        || daemon_public_path(req.uri().path())
        || daemon_presented_token(req.headers())
            .is_some_and(|token| api_tokens.iter().any(|allowed| allowed == &token))
    {
        return next.run(req).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"code":"UNAUTHORIZED","message":"missing or invalid API token"}"#,
    )
        .into_response()
}

fn daemon_public_path(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/api/v1/auth/login"
            | "/api/v1/auth/logout"
            | "/api/qb/v2/auth/login"
            | "/api/qb/v2/auth/logout"
            | "/api/v2/auth/login"
            | "/api/v2/auth/logout"
    ) || is_webui_path(path)
}

fn daemon_presented_token(headers: &HeaderMap) -> Option<String> {
    bearer_token(headers)
        .or_else(|| session_cookie(headers, "tng_session"))
        .or_else(|| session_cookie(headers, "SID"))
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn session_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let prefix = format!("{cookie_name}=");
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| {
            cookie.split(';').find_map(|part| {
                let part = part.trim();
                part.strip_prefix(&prefix).and_then(cookie_component_decode)
            })
        })
}

fn cookie_component_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = hex_value(bytes[index + 1])?;
        let low = hex_value(bytes[index + 2])?;
        output.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

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
    let remote_addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    let request_id = request_id(req.headers());
    let started = Instant::now();
    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-request-id"), value);
    }
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
        request_id = %request_id,
        method = %method,
        path = %path,
        route = route.as_deref(),
        remote_addr = remote_addr.map(|addr| addr.to_string()).as_deref(),
        status = status.as_u16(),
        duration_ms,
        response_size,
        result = if status.is_server_error() { "error" } else { "ok" },
        "http request completed"
    );
    response
}

fn request_id(headers: &HeaderMap) -> String {
    rt_logging::correlation_id(
        headers
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        || format!("tng-{}", REQUEST_ID.fetch_add(1, Ordering::Relaxed)),
    )
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

fn is_webui_path(path: &str) -> bool {
    !path.starts_with("/api/") && path != "/metrics" && path != "/ws"
}

fn is_static_asset_path(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("css" | "js" | "map" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico")
    )
}

fn load_config() -> anyhow::Result<Config> {
    if let Ok(path) = std::env::var("TORRENTNGD_CONFIG") {
        return Config::load(std::path::Path::new(&path))
            .with_context(|| format!("loading explicit config from {path}"));
    }
    Config::load_default().context("loading default config")
}

#[cfg(test)]
mod tests {
    use super::{daemon_public_path, request_id, skip_request_log, static_dir};
    use axum::http::{HeaderMap, HeaderValue};

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

    #[test]
    fn daemon_auth_allows_webui_but_keeps_api_private() {
        for path in [
            "/",
            "/index.html",
            "/assets/app.js",
            "/favicon.ico",
            "/torrents/abc123",
            "/health",
            "/api/v1/auth/login",
            "/api/v1/auth/logout",
        ] {
            assert!(daemon_public_path(path), "{path}");
        }

        for path in [
            "/api/v1/torrents",
            "/api/qb/v2/torrents/info",
            "/metrics",
            "/ws",
        ] {
            assert!(!daemon_public_path(path), "{path}");
        }
    }

    #[test]
    fn static_dir_defaults_to_packaged_webui_path() {
        assert_eq!(
            static_dir(),
            std::path::PathBuf::from("/usr/share/torrentng/webui")
        );
    }

    #[test]
    fn request_id_accepts_bounded_safe_header_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-request-id",
            HeaderValue::from_static("client-123.trace/4"),
        );
        assert_eq!(request_id(&headers), "client-123.trace/4");

        headers.insert("x-request-id", HeaderValue::from_static("bad value"));
        assert!(request_id(&headers).starts_with("tng-"));

        headers.insert("x-request-id", HeaderValue::from_static(""));
        assert!(request_id(&headers).starts_with("tng-"));
    }
}
