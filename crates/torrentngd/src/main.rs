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
use tokio::sync::{oneshot, Notify, RwLock, Semaphore};
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

use rt_api_deluge::AppState as DelugeState;
use rt_api_model::{
    api_token_allowed, bearer_token as model_bearer_token, csrf_request_allowed,
    has_browser_request_headers, session_cookie_value, ApiRuntimeMetrics,
    MAX_API_URI_BYTES as MAX_REQUEST_URI_BYTES,
};
use rt_api_native::state::{AppState as TorrentNgApiState, AuthCredentials};
use rt_api_qbit::state::AppState as QbitState;
use rt_api_transmission::AppState as TransmissionState;
use rt_config::Config;
use rt_engine::Engine;
use rt_session::SessionRegistry;

mod export;
mod migrate;

// jemalloc handles the daemon's real allocation shape — many concurrent
// per-torrent tasks doing small, high-churn allocations (peer buffers,
// piece-map/picker state, DB rows) over long uptimes — better than glibc's
// allocator, which fragments and grows RSS under sustained small-object
// churn. Background threads purge freed arenas back to the OS instead of
// holding them, which is what actually keeps long-run memory flat.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Bound request handlers that can fan out into engine/database work. The
/// limit is deliberately enforced at the daemon boundary so all mounted API
/// facades share one budget instead of each compatibility router admitting
/// its own workload.
const MAX_CONCURRENT_DAEMON_REQUESTS: usize = 256;

#[derive(Clone)]
struct DaemonAuthGate {
    api_tokens: Arc<Vec<String>>,
    local_webui_session_token: Option<String>,
}

fn install_panic_payload_redacting_hook() {
    use std::io::Write as _;

    std::panic::set_hook(Box::new(|panic| {
        let mut stderr = std::io::stderr().lock();
        if let Some(location) = panic.location() {
            let _ = writeln!(
                stderr,
                "TorrentNG panic at {}:{}:{}; panic payload omitted",
                location.file(),
                location.line(),
                location.column()
            );
        } else {
            let _ = writeln!(stderr, "TorrentNG panic; panic payload omitted");
        }
    }));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_payload_redacting_hook();
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
                .map_err(|error| {
                    anyhow::Error::msg(rt_engine::task_join_error_summary(
                        "migrate command",
                        &error,
                    ))
                })?;
        }
        Some("export") => {
            let rest = argv[2..].to_vec();
            return tokio::task::spawn_blocking(move || export::run(&rest))
                .await
                .map_err(|error| {
                    anyhow::Error::msg(rt_engine::task_join_error_summary("export command", &error))
                })?;
        }
        Some("auth-token") => {
            let config = load_config()?;
            let (_, credentials, _) = resolve_auth_credentials(&config)?;
            println!("WebUI username: {}", credentials.username);
            println!("WebUI password: {}", credentials.password);
            println!("Configured API tokens can also be used in either login field.");
            return Ok(());
        }
        _ => {}
    }

    let config = Arc::new(load_config()?);
    let api_addr: SocketAddr = config
        .daemon
        .api_bind
        .parse()
        .context("invalid api_bind address")?;
    let public_bind = !api_addr.ip().is_loopback();
    let local_webui_session_token = if config.auth.api_tokens.is_empty() {
        Some(format!("tng-local-{}", uuid::Uuid::new_v4().simple()))
    } else {
        None
    };
    rt_logging::init(&config.logging, Some(&config.daemon.log_level));
    if config.metrics.include_torrent_ids {
        tracing::warn!(
            component = "metrics",
            operation = "startup",
            "raw torrent identifiers are enabled in Prometheus labels; prefer the default hashed labels"
        );
    }
    info!(
        component = "daemon",
        operation = "startup",
        version = env!("CARGO_PKG_VERSION"),
        api_bind = %config.daemon.api_bind,
        listen_port = config.network.listen_port,
        "torrentngd starting"
    );

    // Ensure session directory exists
    rt_storage::create_dir_all_no_follow(&config.daemon.session_dir)
        .with_context(|| format!("creating session_dir {:?}", config.daemon.session_dir))?;

    let (configured_auth_credentials, auth_credentials, auth_settings_path) =
        resolve_auth_credentials(&config)?;

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
    let mut torrentng_api_state = TorrentNgApiState::with_engine_and_tokens_metrics_config(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
        Arc::clone(&api_metrics),
        config.metrics.include_torrent_ids,
    );
    torrentng_api_state.configure_auth_credentials(
        auth_credentials,
        configured_auth_credentials,
        auth_settings_path,
        public_bind,
        local_webui_session_token.clone(),
    );
    let torrentng_api_router = rt_api_native::router::build_router(torrentng_api_state);

    let shutdown_notify = Arc::new(Notify::new());
    let mut qbit_state = QbitState::with_engine_and_tokens_and_metrics(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
        Arc::clone(&api_metrics),
    );
    qbit_state.egress_policy = rt_engine::OutboundEgressPolicy::from_config(&config.tracker);
    qbit_state.shutdown = Some(Arc::clone(&shutdown_notify));
    let qbit_router = rt_api_qbit::router::build_qbit_router(qbit_state);

    let mut transmission_state = TransmissionState::with_engine_and_tokens(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
    );
    if let Err(error) = transmission_state.restore_persisted_state().await {
        engine_handle.shutdown().await;
        return Err(anyhow::Error::msg(error).context("restoring Transmission compatibility state"));
    }
    transmission_state.shutdown = Some(Arc::clone(&shutdown_notify));
    let transmission_router = rt_api_transmission::build_transmission_router(transmission_state);

    let mut deluge_state = DelugeState::with_engine_and_tokens(
        Arc::clone(&registry),
        engine_handle.clone(),
        config.auth.api_tokens.clone(),
    );
    deluge_state.shutdown = Some(Arc::clone(&shutdown_notify));
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

    let app = torrentng_api_router
        .merge(qbit_router)
        .merge(transmission_router)
        .merge(deluge_router)
        .fallback_service(
            ServeDir::new(&static_dir).not_found_service(ServeFile::new(&static_index)),
        )
        .layer(middleware::from_fn(request_uri_guard))
        .layer(middleware::from_fn(request_log))
        .layer(middleware::from_fn_with_state(
            DaemonAuthGate {
                api_tokens: Arc::new(config.auth.api_tokens.clone()),
                local_webui_session_token: local_webui_session_token.clone(),
            },
            daemon_auth_guard,
        ))
        .layer(middleware::from_fn_with_state(
            Arc::new(Semaphore::new(MAX_CONCURRENT_DAEMON_REQUESTS)),
            request_concurrency_guard,
        ));

    info!(
        component = "http",
        operation = "listen",
        addr = %api_addr,
        "API listening"
    );

    let listener = match tokio::net::TcpListener::bind(api_addr)
        .await
        .with_context(|| format!("binding API to {api_addr}"))
    {
        Ok(listener) => listener,
        Err(error) => {
            engine_handle.shutdown().await;
            return Err(error);
        }
    };

    let (shutdown_started_tx, shutdown_started_rx) = oneshot::channel();
    let api_shutdown_started = Arc::new(Notify::new());
    let shutdown_started_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let shutdown_task = tokio::spawn(shutdown_signal(
        engine_handle.clone(),
        shutdown_notify,
        shutdown_started_tx,
        Arc::clone(&api_shutdown_started),
        Arc::clone(&shutdown_started_flag),
    ));
    let api_shutdown_grace =
        std::time::Duration::from_secs(config.daemon.shutdown_timeout_secs.max(1));
    let mut serve = Box::pin(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_started_rx.await;
        })
        .await
    });
    let serve_result = tokio::select! {
        result = &mut serve => result,
        _ = async {
            api_shutdown_started.notified().await;
            tokio::time::sleep(api_shutdown_grace).await;
        } => {
            tracing::warn!(
                component = "http",
                operation = "shutdown",
                result = "timeout",
                timeout_secs = api_shutdown_grace.as_secs(),
                "API connections did not drain before the shutdown deadline"
            );
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "API graceful shutdown timed out",
            ))
        }
    };
    // A timed-out server future owns the active connection tasks. Drop it
    // before waiting on the engine so long-lived SSE/WebSocket clients cannot
    // keep the daemon process alive after the API deadline.
    drop(serve);
    if serve_result.is_err() && !shutdown_started_flag.load(Ordering::Acquire) {
        // A server failure can occur without a signal. Do not leave the
        // signal waiter detached while the main task performs the fallback
        // engine shutdown below.
        shutdown_task.abort();
    }
    // The signal task starts engine shutdown before Axum drains existing
    // connections, then this idempotent call ensures the main task joins the
    // same bounded shutdown operation on both normal and error exits.
    engine_handle.shutdown().await;
    let _ = shutdown_task.await;
    if let Err(error) = serve_result {
        return Err(anyhow::Error::new(error).context("API server error"));
    }

    Ok(())
}

async fn shutdown_signal(
    engine: rt_engine::EngineHandle,
    shutdown_notify: Arc<Notify>,
    shutdown_started: oneshot::Sender<()>,
    api_shutdown_started: Arc<Notify>,
    shutdown_started_flag: Arc<std::sync::atomic::AtomicBool>,
) {
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
            _ = shutdown_notify.notified() => {
                info!(
                    component = "daemon",
                    operation = "shutdown_signal",
                    signal = "qbit-app-shutdown",
                    "received qBittorrent application shutdown request"
                );
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!(
                    component = "daemon",
                    operation = "shutdown_signal",
                    signal = "ctrl-c",
                    "received shutdown signal"
                );
            }
            _ = shutdown_notify.notified() => {
                info!(
                    component = "daemon",
                    operation = "shutdown_signal",
                    signal = "qbit-app-shutdown",
                    "received qBittorrent application shutdown request"
                );
            }
        }
    }
    shutdown_started_flag.store(true, Ordering::Release);
    api_shutdown_started.notify_one();
    let _ = shutdown_started.send(());
    engine.shutdown().await;
}

fn print_help() {
    println!(
        "torrentngd {}\n\nUSAGE:\n    torrentngd [migrate|export] [OPTIONS]\n\nENV:\n    TORRENTNGD_CONFIG  Path to TorrentNG client config\n    TNG_STATIC_DIR     Built WebUI directory to serve, default /usr/share/torrentng/webui\n\nCOMMANDS:\n    migrate            Import existing client state into the TorrentNG client\n    export             Export TorrentNG client state for another client\n    help               Print this help\n    version            Print version",
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
    State(auth): State<DaemonAuthGate>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if auth.api_tokens.is_empty() {
        if daemon_public_path(path) {
            if daemon_public_auth_path(path)
                && daemon_is_mutating(&req)
                && has_browser_request_headers(req.headers())
                && !csrf_request_allowed(req.headers())
            {
                return (
                    StatusCode::FORBIDDEN,
                    "cross-origin browser authentication request rejected",
                )
                    .into_response();
            }
            return next.run(req).await;
        }

        let local_session_valid = auth
            .local_webui_session_token
            .as_ref()
            .is_some_and(|expected| daemon_local_session_valid(req.headers(), expected));
        if path == "/api/v1/auth/settings" && !local_session_valid {
            return daemon_unauthorized();
        }
        if has_browser_request_headers(req.headers()) {
            if !local_session_valid {
                return daemon_unauthorized();
            }
            if daemon_is_mutating(&req) && !csrf_request_allowed(req.headers()) {
                return (StatusCode::FORBIDDEN, "cross-site cookie mutation rejected")
                    .into_response();
            }
        }
        if daemon_is_mutating(&req)
            && has_browser_request_headers(req.headers())
            && !csrf_request_allowed(req.headers())
        {
            return (
                StatusCode::FORBIDDEN,
                "cross-origin browser request rejected",
            )
                .into_response();
        }
        return next.run(req).await;
    }

    if bearer_token(req.headers()).is_some_and(|token| api_token_allowed(&auth.api_tokens, &token))
    {
        return next.run(req).await;
    }
    if daemon_public_path(path) {
        if daemon_public_auth_path(path)
            && daemon_is_mutating(&req)
            && has_browser_request_headers(req.headers())
            && !csrf_request_allowed(req.headers())
        {
            return (
                StatusCode::FORBIDDEN,
                "cross-origin browser authentication request rejected",
            )
                .into_response();
        }
        return next.run(req).await;
    }
    if session_cookie_value(req.headers(), &["tng_session", "SID"])
        .is_some_and(|token| api_token_allowed(&auth.api_tokens, &token))
    {
        if daemon_is_mutating(&req) && !csrf_request_allowed(req.headers()) {
            return (StatusCode::FORBIDDEN, "cross-site cookie mutation rejected").into_response();
        }
        return next.run(req).await;
    }

    daemon_unauthorized()
}

fn daemon_local_session_valid(headers: &HeaderMap, expected: &str) -> bool {
    session_cookie_value(headers, &["tng_session"])
        .is_some_and(|token| api_token_allowed(std::slice::from_ref(&expected.to_owned()), &token))
}

fn daemon_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"code":"UNAUTHORIZED","message":"missing or invalid API token"}"#,
    )
        .into_response()
}

/// Keep request-driven engine fan-out bounded across all API facades. A
/// `try_acquire` is intentional: when the daemon is saturated, reject new
/// work immediately instead of creating another unbounded queue of HTTP
/// futures waiting for the same engine/database resources.
async fn request_concurrency_guard(
    State(limiter): State<Arc<Semaphore>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if request_concurrency_bypass(req.uri().path()) {
        return next.run(req).await;
    }

    let Ok(_permit) = limiter.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "daemon request capacity exhausted; retry later",
        )
            .into_response();
    };
    next.run(req).await
}

fn request_concurrency_bypass(path: &str) -> bool {
    // These responses intentionally outlive ordinary request work. The SSE
    // handler has its own client accounting, and `/ws` is reserved for the
    // websocket compatibility surface.
    path == "/api/v1/events" || path == "/ws"
}

fn daemon_public_path(path: &str) -> bool {
    matches!(
        path,
        "/health"
            | "/api/v1/auth/login"
            | "/api/v1/auth/logout"
            | "/api/qb/v2/auth/login"
            | "/api/qb/v2/auth/logout"
            | "/api/qb/v2/app/version"
            | "/api/qb/v2/app/webapiVersion"
            | "/api/v2/auth/login"
            | "/api/v2/auth/logout"
            | "/api/v2/app/version"
            | "/api/v2/app/webapiVersion"
    ) || is_webui_path(path)
}

fn daemon_public_auth_path(path: &str) -> bool {
    matches!(
        path,
        "/api/v1/auth/login"
            | "/api/v1/auth/logout"
            | "/api/qb/v2/auth/login"
            | "/api/qb/v2/auth/logout"
            | "/api/v2/auth/login"
            | "/api/v2/auth/logout"
    )
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    model_bearer_token(headers)
}

fn daemon_is_mutating(req: &Request<Body>) -> bool {
    matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    )
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

async fn request_uri_guard(req: Request<Body>, next: Next) -> Response {
    if !request_uri_is_bounded(req.uri()) {
        return (
            StatusCode::URI_TOO_LONG,
            "request URI exceeds the maximum length",
        )
            .into_response();
    }
    next.run(req).await
}

fn request_uri_is_bounded(uri: &axum::http::Uri) -> bool {
    let uri_bytes = uri
        .path()
        .len()
        .saturating_add(uri.query().map_or(0, |query| query.len().saturating_add(1)));
    uri_bytes <= MAX_REQUEST_URI_BYTES
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

fn resolve_auth_credentials(
    config: &Config,
) -> anyhow::Result<(AuthCredentials, AuthCredentials, PathBuf)> {
    rt_storage::create_dir_all_no_follow(&config.daemon.session_dir).with_context(|| {
        format!(
            "creating auth state directory {:?}",
            config.daemon.session_dir
        )
    })?;
    let auth_settings_path = config.daemon.session_dir.join("auth-settings.json");
    let password = if config.auth.password.is_empty() {
        load_or_create_bootstrap_password(&config.daemon.session_dir.join("bootstrap-password"))?
    } else {
        config.auth.password.clone()
    };
    let configured = AuthCredentials {
        username: config.auth.username.clone(),
        password,
    };
    let active = load_auth_credentials(&auth_settings_path, &configured)?;
    Ok((configured, active, auth_settings_path))
}

const MAX_BOOTSTRAP_PASSWORD_BYTES: usize = 128;

fn load_or_create_bootstrap_password(path: &std::path::Path) -> anyhow::Result<String> {
    match rt_storage::read_file_no_follow_limited(path, MAX_BOOTSTRAP_PASSWORD_BYTES) {
        Ok(bytes) => return validate_bootstrap_password(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading bootstrap password from {}", path.display()))
        }
    }

    let password = uuid::Uuid::new_v4().simple().to_string();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(password.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(password)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_bootstrap_password(rt_storage::read_file_no_follow_limited(
                path,
                MAX_BOOTSTRAP_PASSWORD_BYTES,
            )?)
        }
        Err(error) => {
            Err(error).with_context(|| format!("creating bootstrap password at {}", path.display()))
        }
    }
}

fn validate_bootstrap_password(bytes: Vec<u8>) -> anyhow::Result<String> {
    let password = String::from_utf8(bytes).context("bootstrap password is not UTF-8")?;
    let password = password.trim().to_owned();
    anyhow::ensure!(
        (16..=MAX_BOOTSTRAP_PASSWORD_BYTES).contains(&password.len()),
        "bootstrap password file has an invalid length"
    );
    Ok(password)
}

fn load_auth_credentials(
    path: &std::path::Path,
    configured: &AuthCredentials,
) -> anyhow::Result<AuthCredentials> {
    const MAX_AUTH_SETTINGS_BYTES: usize = 4096;
    let credentials = match rt_storage::read_file_no_follow_limited(path, MAX_AUTH_SETTINGS_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing WebUI auth settings from {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => configured.clone(),
        Err(error) => Err(error)
            .with_context(|| format!("reading WebUI auth settings from {}", path.display()))?,
    };
    anyhow::ensure!(
        !credentials.username.trim().is_empty() && credentials.username.len() <= 256,
        "WebUI username in {} must contain 1-256 bytes",
        path.display()
    );
    anyhow::ensure!(
        credentials.password.trim().len() >= 8 && credentials.password.len() <= 1024,
        "WebUI password in {} must contain at least 8 and at most 1024 bytes",
        path.display()
    );
    Ok(credentials)
}

#[cfg(test)]
mod tests {
    use super::{
        bearer_token, daemon_auth_guard, daemon_public_auth_path, daemon_public_path,
        install_panic_payload_redacting_hook, load_or_create_bootstrap_password, request_id,
        request_uri_is_bounded, skip_request_log, static_dir, DaemonAuthGate,
        MAX_REQUEST_URI_BYTES,
    };
    use axum::{
        body::Body,
        http::{header, HeaderMap, HeaderValue, Request, StatusCode, Uri},
        middleware,
        routing::get,
        Router,
    };
    use std::process::Command;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn bootstrap_password_is_unique_persistent_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("first").join("bootstrap-password");
        let second_path = dir.path().join("second").join("bootstrap-password");
        std::fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();

        let first = load_or_create_bootstrap_password(&first_path).unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            load_or_create_bootstrap_password(&first_path).unwrap(),
            first
        );

        let second = load_or_create_bootstrap_password(&second_path).unwrap();
        assert_ne!(first, second);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

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
    fn request_uri_limit_counts_path_and_query_bytes() {
        let short = Uri::from_static("/api/v1/torrents?limit=50");
        assert!(request_uri_is_bounded(&short));

        let oversized = format!(
            "/api/v1/torrents?filter={}",
            "x".repeat(MAX_REQUEST_URI_BYTES)
        );
        let oversized = oversized.parse::<Uri>().unwrap();
        assert!(!request_uri_is_bounded(&oversized));
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
            "/api/qb/v2/app/version",
            "/api/qb/v2/app/webapiVersion",
            "/api/v2/app/version",
            "/api/v2/app/webapiVersion",
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

    #[tokio::test]
    async fn loopback_default_auth_gates_browser_requests_and_preserves_machine_api_access() {
        let app = Router::new()
            .route("/", get(|| async { "webui" }))
            .route("/api/v1/torrents", get(|| async { "torrents" }))
            .route("/api/v1/auth/settings", get(|| async { "settings" }))
            .layer(middleware::from_fn_with_state(
                DaemonAuthGate {
                    api_tokens: Arc::new(Vec::new()),
                    local_webui_session_token: Some("local-session".to_owned()),
                },
                daemon_auth_guard,
            ));

        let webui = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(webui.status(), StatusCode::OK);

        let machine = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(machine.status(), StatusCode::OK);

        let unauthenticated_browser = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .header("sec-fetch-site", "same-origin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated_browser.status(), StatusCode::UNAUTHORIZED);

        let settings_without_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(settings_without_session.status(), StatusCode::UNAUTHORIZED);

        let authenticated_browser = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/torrents")
                    .header("sec-fetch-site", "same-origin")
                    .header("cookie", "tng_session=local-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated_browser.status(), StatusCode::OK);
    }

    #[test]
    fn daemon_public_auth_paths_are_exact() {
        for path in [
            "/api/v1/auth/login",
            "/api/v1/auth/logout",
            "/api/qb/v2/auth/login",
            "/api/qb/v2/auth/logout",
            "/api/v2/auth/login",
            "/api/v2/auth/logout",
        ] {
            assert!(daemon_public_auth_path(path), "{path}");
        }
        for path in [
            "/health",
            "/api/v1/auth/refresh",
            "/api/qb/v2/auth/login/extra",
            "/api/v2/torrents/info",
        ] {
            assert!(!daemon_public_auth_path(path), "{path}");
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

    #[test]
    fn bearer_token_accepts_case_insensitive_scheme_without_extra_parts() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "bEaReR secret".parse().unwrap());
        assert_eq!(bearer_token(&headers).as_deref(), Some("secret"));

        headers.insert(header::AUTHORIZATION, "Basic secret".parse().unwrap());
        assert!(bearer_token(&headers).is_none());
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret extra".parse().unwrap(),
        );
        assert!(bearer_token(&headers).is_none());
    }

    #[test]
    #[ignore = "subprocess entrypoint for the panic-hook regression"]
    fn panic_hook_child_entrypoint() {
        install_panic_payload_redacting_hook();
        panic!("panic-hook-canary");
    }

    #[test]
    fn panic_hook_omits_payload_but_keeps_location_in_stderr() {
        let executable = std::env::current_exe().unwrap();
        let output = Command::new(executable)
            .args([
                "--exact",
                "tests::panic_hook_child_entrypoint",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("panic payload omitted"), "{stderr}");
        assert!(stderr.contains("main.rs:"), "{stderr}");
        assert!(!stderr.contains("panic-hook-canary"), "{stderr}");
    }
}
