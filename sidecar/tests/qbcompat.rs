//! Integration tests for the qBittorrent compatibility layer.
//!
//! These tests spin up the full axum router against an in-memory SQLite DB.
//! They do NOT require a running rTorrent instance — endpoints that touch rTorrent
//! are skipped unless RTORRENT_SOCKET is set.

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use reqwest::Client;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};

// Re-use internal modules via the binary crate root.
use torrentng::{
    api::{server::AppState, ws::Event},
    backend::{
        BackendCapabilities, BackendStatus, BackendTransferLimits, BackendType, TorrentBackend,
    },
    cache::{AppEventRow, Db, TorrentRow},
    config::Config,
    metrics::Metrics,
    rtorrent::{
        files::RawFile,
        torrents::{RawTorrent, TransferRates},
        trackers::RawTracker,
    },
};

async fn spawn_server() -> (SocketAddr, Client) {
    let (addr, client, _) = spawn_server_with_db().await;
    (addr, client)
}

async fn spawn_server_with_db() -> (SocketAddr, Client, Arc<Db>) {
    spawn_server_with_config(Config::test_default()).await
}

async fn spawn_server_with_config(cfg: Config) -> (SocketAddr, Client, Arc<Db>) {
    let (addr, client, db, _) = spawn_server_with_config_and_events(cfg).await;
    (addr, client, db)
}

async fn spawn_server_with_config_and_events(
    cfg: Config,
) -> (SocketAddr, Client, Arc<Db>, broadcast::Sender<Event>) {
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let backend = Arc::new(torrentng::backend::rtorrent::RtorrentBackend::new(
        rt.clone(),
    ));
    spawn_server_with_backend_and_events(cfg, rt, backend).await
}

async fn spawn_server_with_backend_and_events(
    cfg: Config,
    rt: Arc<torrentng::rtorrent::Client>,
    backend: Arc<dyn TorrentBackend>,
) -> (SocketAddr, Client, Arc<Db>, broadcast::Sender<Event>) {
    let cfg = Arc::new(cfg);
    let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let db = Arc::new(Db::open(db_path.as_ref()).unwrap());
    let (tx, _) = broadcast::channel::<Event>(16);
    let metrics = Metrics::new();

    let state = AppState {
        cfg,
        rt,
        backend,
        db: db.clone(),
        events: tx.clone(),
        metrics,
        qbit_search_plugins: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_search_jobs: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_next_search_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        qbit_rss_items: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        control_plane_write: Arc::new(tokio::sync::Mutex::new(())),
    };
    let app: Router = torrentng::api::server::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::builder().cookie_store(true).build().unwrap();

    (addr, client, db, tx)
}

async fn spawn_server_with_backend(
    cfg: Config,
    backend: Arc<dyn TorrentBackend>,
) -> (SocketAddr, Client, Arc<Db>) {
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let (addr, client, db, _) = spawn_server_with_backend_and_events(cfg, rt, backend).await;
    (addr, client, db)
}

/// Successful backend used by mutation-flow tests. The production handlers
/// deliberately update the cache only after the backend accepts a mutation;
/// using the unreachable rTorrent stub here would test the failure path, not
/// compatibility behavior.
struct SuccessfulBackend;

#[async_trait::async_trait]
impl TorrentBackend for SuccessfulBackend {
    fn backend_type(&self) -> BackendType {
        BackendType::Torrentng
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_tags: true,
            supports_categories: true,
            supports_file_priority: true,
            supports_tracker_edit: true,
            supports_recheck: true,
            supports_torrent_export: true,
            supports_webseed_reads: true,
            supports_piece_state_reads: true,
            supports_piece_hash_reads: true,
            supports_peer_snapshots: true,
            supports_peer_add: true,
            supports_peer_ban: true,
            supports_queue_order: true,
            supports_per_torrent_limits: true,
            supports_global_limits: true,
            supports_share_limits: true,
            supports_mode_flags: true,
            supports_location_update: true,
            supports_torrent_rename: true,
            supports_file_rename: true,
            supports_runtime_user_agent: true,
            supports_config_overlay: true,
            supports_restart: true,
        }
    }

    async fn health(&self) -> BackendStatus {
        BackendStatus::Connected
    }

    async fn transfer_rates(&self) -> anyhow::Result<TransferRates> {
        Ok(TransferRates::default())
    }

    async fn list_torrents(&self) -> anyhow::Result<Vec<RawTorrent>> {
        Ok(Vec::new())
    }

    async fn add_magnet(
        &self,
        _magnet: &str,
        _save_path: &str,
        _category: &str,
        _start: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn add_torrent(
        &self,
        _data: &[u8],
        _save_path: &str,
        _category: &str,
        _start: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove(&self, _hash: &str, _delete_data: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start(&self, _hash: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop(&self, _hash: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recheck(&self, _hash: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reannounce(&self, _hash: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_trackers(&self, _hash: &str) -> anyhow::Result<Vec<RawTracker>> {
        Ok(Vec::new())
    }

    async fn add_tracker(&self, _hash: &str, _url: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn edit_tracker(
        &self,
        _hash: &str,
        _original_url: &str,
        _new_url: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_tracker(&self, _hash: &str, _url: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_files(&self, _hash: &str) -> anyhow::Result<Vec<RawFile>> {
        Ok(Vec::new())
    }

    async fn set_file_priority(
        &self,
        _hash: &str,
        _file_index: usize,
        _priority: i64,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_category(&self, _hash: &str, _category: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_location(&self, _hash: &str, _location: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_share_limits(
        &self,
        _hash: &str,
        _ratio_limit_milli: i64,
        _seeding_time_limit: i64,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_force_start(&self, _hash: &str, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_super_seeding(&self, _hash: &str, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_auto_tmm(&self, _hash: &str, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_auto_management(&self, _hash: &str, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_dht(&self, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_pex(&self, _enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_user_agent(&self) -> anyhow::Result<String> {
        Ok("TorrentNG-Test/1.0".to_owned())
    }

    async fn set_user_agent(&self, _user_agent: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn global_limits(&self) -> anyhow::Result<BackendTransferLimits> {
        Ok(BackendTransferLimits::default())
    }

    async fn add_tags(&self, _hash: &str, _tags: &[&str]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_tags(&self, _hash: &str, _tags: &[&str]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_tags(&self, _hash: &str, _tags: &[&str]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn has_bounded_sync(&self) -> bool {
        true
    }
}

fn successful_backend() -> Arc<dyn TorrentBackend> {
    Arc::new(SuccessfulBackend)
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

async fn spawn_webhook_receiver() -> (SocketAddr, mpsc::Receiver<serde_json::Value>) {
    let (tx, rx) = mpsc::channel::<serde_json::Value>(8);
    async fn capture(
        State(tx): State<mpsc::Sender<serde_json::Value>>,
        Json(body): Json<serde_json::Value>,
    ) -> StatusCode {
        let _ = tx.send(body).await;
        StatusCode::NO_CONTENT
    }

    let app = Router::new().route("/hook", post(capture)).with_state(tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, rx)
}

async fn assert_event(rx: &mut broadcast::Receiver<Event>, expected: &str) {
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let raw = serde_json::to_value(event).unwrap();
    assert_eq!(raw["type"], expected);
}

fn seed_torrent(db: &Db, hash: &str, name: &str) {
    seed_torrent_with(db, hash, name, |_| {});
}

fn seed_torrent_with(db: &Db, hash: &str, name: &str, mutate: impl FnOnce(&mut TorrentRow)) {
    let mut row = TorrentRow {
        hash: hash.to_owned(),
        name: name.to_owned(),
        size_bytes: 100,
        bytes_done: 0,
        down_rate: 0,
        up_rate: 0,
        up_total: 0,
        down_total: 0,
        ratio: 0,
        is_active: false,
        is_open: false,
        complete: false,
        state: 0,
        priority: 0,
        category: String::new(),
        base_path: String::new(),
        directory: String::new(),
        creation_date: 0,
        timestamp_finished: 0,
        tracker_focus: 0,
        peers_connected: 0,
        peers_complete: 0,
        message: String::new(),
        tracker_url: String::new(),
        tags: String::new(),
        updated_at: 1,
    };
    mutate(&mut row);
    db.upsert(&row).unwrap();
}

// --- qBit auth ---

#[tokio::test]
async fn qb_login_accepts_any_credentials() {
    let (addr, client) = spawn_server().await;
    let res = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Ok.");
}

#[tokio::test]
async fn qb_login_sets_session_cookie_for_api_token() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secret-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    let res = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Fails.");

    let res = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "secret-token")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Ok.");

    let res = client
        .get(url(addr, "/api/qb/v2/app/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn qb_canonical_api_v2_login_is_public() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secret-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .post(url(addr, "/api/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "secret-token")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Ok.");
}

#[tokio::test]
async fn qb_version_probes_are_public_for_arr_clients() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secret-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    for endpoint in [
        "/api/qb/v2/app/version",
        "/api/qb/v2/app/webapiVersion",
        "/api/v2/app/version",
        "/api/v2/app/webapiVersion",
    ] {
        let res = client.get(url(addr, endpoint)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{endpoint}");
    }
}

#[tokio::test]
async fn qb_sid_cookie_authorizes_requests() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secret-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .get(url(addr, "/api/v2/app/version"))
        .header("Cookie", "SID=secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn metrics_requires_auth_when_tokens_are_configured() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["sidecar-api-token-20260904".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client.get(url(addr, "/metrics")).send().await.unwrap();
    assert_eq!(res.status(), 401);

    let res = client
        .get(url(addr, "/metrics"))
        .bearer_auth("sidecar-api-token-20260904")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn trusted_proxy_header_authorizes_only_when_explicitly_enabled() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["sidecar-api-token-20260904".to_owned()];
    cfg.auth.trust_proxy_header = true;
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .header("X-Remote-User", "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .header("X-Remote-User", "   ")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn unknown_auth_routes_are_not_public() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["sidecar-api-token-20260904".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .get(url(addr, "/api/v2/auth/future-admin-operation"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn signed_session_cookie_does_not_contain_the_api_token() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["sidecar-api-token-20260904".to_owned()];
    cfg.auth.secret_key = Some("sidecar-session-secret-20260904-0123456789abcdef".to_owned());
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[
            ("username", "admin"),
            ("password", "sidecar-api-token-20260904"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let cookies = res
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(cookies.len(), 2);
    assert!(cookies
        .iter()
        .all(|cookie| !cookie.contains("sidecar-api-token-20260904")));

    let res = client
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn qb_app_read_endpoints_accept_post_for_cross_seed() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secret-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .post(url(addr, "/api/v2/app/version"))
        .header("Cookie", "SID=secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "5.0.0");
}

// --- qBit app ---

#[tokio::test]
async fn qb_version() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/app/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn qb_api_version() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/app/webapiVersion"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn qb_canonical_api_v2_alias() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/v2/app/webapiVersion"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn qb_app_extra_info() {
    let (addr, client) = spawn_server().await;

    let res = client
        .get(url(addr, "/api/qb/v2/app/buildInfo"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["bitness"], 64);

    let res = client
        .get(url(addr, "/api/qb/v2/app/defaultSavePath"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "/data/downloads");
}

// --- Categories / Tags (DB-backed, no rTorrent needed) ---

#[tokio::test]
async fn categories_round_trip() {
    let (addr, client) = spawn_server().await;

    // Start empty
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/categories"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_object().unwrap().is_empty());

    // Create via native API
    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "Movies", "save_path": "/data/movies" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // Appears in qBit categories
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/categories"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("Movies").is_some());
    assert_eq!(body["Movies"]["savePath"], "/data/movies");

    // Delete via native API
    let res = client
        .delete(url(addr, "/api/v1/categories/Movies"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    // Gone
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/categories"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn deleting_category_clears_cached_torrent_category() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "cat-hash", "Categorized");

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "Movies", "save_path": "/data/movies" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .put(url(addr, "/api/v1/torrents/cat-hash/category"))
        .json(&serde_json::json!({ "category": "Movies" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    let res = client
        .delete(url(addr, "/api/v1/categories/Movies"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    let res = client
        .get(url(addr, "/api/v1/torrents/cat-hash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["category"], "");
}

#[tokio::test]
async fn native_category_and_tag_names_are_validated() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "  ", "save_path": "/tmp" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/tags"))
        .json(&serde_json::json!({ "name": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/torrents/hash/tags"))
        .json(&serde_json::json!({ "tags": [" ", ""] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn native_torrent_metadata_updates_404_for_missing_torrent() {
    let (addr, client) = spawn_server().await;

    let res = client
        .put(url(addr, "/api/v1/torrents/missing/category"))
        .json(&serde_json::json!({ "category": "Movies" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .post(url(addr, "/api/v1/torrents/missing/tags"))
        .json(&serde_json::json!({ "tags": ["new"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .delete(url(addr, "/api/v1/torrents/missing/tags"))
        .json(&serde_json::json!({ "tags": ["new"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn tags_round_trip() {
    let (addr, client) = spawn_server().await;

    // Empty initially
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/tags"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<String> = res.json().await.unwrap();
    assert!(body.is_empty());

    // Create tag
    let res = client
        .post(url(addr, "/api/v1/tags"))
        .json(&serde_json::json!({ "name": "4k" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);

    // Appears in qBit tags
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/tags"))
        .send()
        .await
        .unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(body.contains(&"4k".to_string()));

    // Delete
    let res = client
        .delete(url(addr, "/api/v1/tags/4k"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/tags"))
        .send()
        .await
        .unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(!body.contains(&"4k".to_string()));
}

// --- Torrent list (empty cache) ---

#[tokio::test]
async fn qb_torrents_info_empty() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/torrents/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn qb_torrents_info_paused_filter_only_returns_inactive() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "paused-hash", "Paused");
    seed_torrent_with(&db, "active-hash", "Active", |t| {
        t.is_active = true;
        t.is_open = true;
        t.complete = false;
    });

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/info?filter=paused"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["hash"], "paused-hash");
}

#[tokio::test]
async fn qb_torrents_info_status_filters_match_cache_state() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "active-down", "Active Down", |t| {
        t.is_active = true;
        t.is_open = true;
        t.complete = false;
    });
    seed_torrent_with(&db, "complete-idle", "Complete Idle", |t| {
        t.complete = true;
    });
    seed_torrent_with(&db, "errored-idle", "Errored Idle", |t| {
        t.message = "tracker error".into();
    });

    let cases = [
        ("completed", vec!["complete-idle"]),
        ("active", vec!["active-down"]),
        ("inactive", vec!["complete-idle", "errored-idle"]),
        ("errored", vec!["errored-idle"]),
    ];

    for (filter, expected) in cases {
        let res = client
            .get(url(
                addr,
                &format!("/api/qb/v2/torrents/info?filter={filter}&sort=name"),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{filter}");
        let body: Vec<serde_json::Value> = res.json().await.unwrap();
        let hashes: Vec<&str> = body.iter().map(|t| t["hash"].as_str().unwrap()).collect();
        assert_eq!(hashes, expected, "{filter}");
    }
}

#[tokio::test]
async fn qb_integration_flow_read_only_clients() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent_with(&db, "mobile-readonly", "Mobile Readonly", |t| {
        t.complete = true;
        t.category = "Movies".into();
        t.directory = "/data/movies".into();
        t.up_rate = 1024;
        t.up_total = 2048;
        t.ratio = 1500;
    });
    db.set_torrent_tags("mobile-readonly", &["4k", "archive"])
        .unwrap();

    for path in [
        "/api/qb/v2/app/version",
        "/api/qb/v2/app/webapiVersion",
        "/api/qb/v2/transfer/info",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
    }
    let res = client
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let prefs: serde_json::Value = res.json().await.unwrap();
    assert!(prefs.as_object().unwrap().contains_key("dht"));
    assert!(prefs.as_object().unwrap().contains_key("pex"));

    let res = client
        .get(url(
            addr,
            "/api/qb/v2/torrents/info?filter=completed&category=Movies&tag=4k",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["hash"], "mobile-readonly");
    assert_eq!(body[0]["category"], "Movies");

    let res = client
        .get(url(
            addr,
            "/api/qb/v2/torrents/properties?hash=mobile-readonly",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["save_path"], "/data/movies");
    assert_eq!(body["share_ratio"], 1.5);
}

#[tokio::test]
async fn qb_integration_flow_arr_category_tag_and_sync() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent_with(&db, "arr-managed", "Arr Managed", |t| {
        t.complete = true;
        t.category = "radarr".into();
        t.updated_at = 10;
    });
    db.set_torrent_tags("arr-managed", &["imported"]).unwrap();

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .form(&[("category", "radarr"), ("savePath", "/data/movies")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/createTags"))
        .form(&[("tags", "imported,upgraded")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], true);
    assert!(body["categories"].get("radarr").is_some());
    assert!(body["tags"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("upgraded")));
    assert!(body["torrents"].get("arr-managed").is_some());

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setTags"))
        .form(&[("hashes", "arr-managed"), ("tags", "imported,upgraded")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/info?tag=upgraded"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["hash"], "arr-managed");
}

#[tokio::test]
async fn qb_integration_flow_cross_seed_tracker_and_reannounce() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent_with(&db, "cross-seed", "Cross Seed", |t| {
        t.complete = true;
        t.tracker_url = "udp://old.example/announce".into();
    });

    for path in [
        "/api/qb/v2/torrents/reannounce",
        "/api/qb/v2/torrents/start",
        "/api/qb/v2/torrents/stop",
        "/api/qb/v2/torrents/setAutoTMM",
    ] {
        let res = client
            .post(url(addr, path))
            .form(&[("hashes", "cross-seed"), ("value", "false")])
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{path}");
    }

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTrackers"))
        .form(&[
            ("hashes", "cross-seed"),
            (
                "urls",
                "udp://new-a.example/announce\nudp://new-b.example/announce",
            ),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/removeTrackers"))
        .form(&[
            ("hash", "cross-seed"),
            ("urls", "udp://old.example/announce"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn websocket_events_emit_for_native_metadata_mutations() {
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let (addr, client, db, tx) =
        spawn_server_with_backend_and_events(Config::test_default(), rt, successful_backend())
            .await;
    let mut rx = tx.subscribe();
    seed_torrent(&db, "event-native", "Event Native");

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "Events", "save_path": "/data/events" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "categories_updated").await;

    let res = client
        .put(url(addr, "/api/v1/torrents/event-native/category"))
        .json(&serde_json::json!({ "category": "Events" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_event(&mut rx, "torrent_updated").await;
    assert_event(&mut rx, "tracker_health_updated").await;
    assert_event(&mut rx, "categories_updated").await;

    let res = client
        .post(url(addr, "/api/v1/torrents/event-native/tags"))
        .json(&serde_json::json!({ "tags": ["fresh"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_event(&mut rx, "torrent_updated").await;
    assert_event(&mut rx, "tracker_health_updated").await;
    assert_event(&mut rx, "tags_updated").await;
}

#[tokio::test]
async fn websocket_events_emit_for_qb_metadata_mutations() {
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let (addr, client, db, tx) =
        spawn_server_with_backend_and_events(Config::test_default(), rt, successful_backend())
            .await;
    let mut rx = tx.subscribe();
    seed_torrent(&db, "event-qb", "Event QB");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/createTags"))
        .form(&[("tags", "qb-event")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "tags_updated").await;

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setTags"))
        .form(&[("hashes", "event-qb"), ("tags", "qb-event")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "torrent_updated").await;
    assert_event(&mut rx, "tracker_health_updated").await;
    assert_event(&mut rx, "tags_updated").await;
}

#[tokio::test]
async fn native_torrents_list_empty() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/v1/torrents"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 0);
    assert!(body["torrents"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn native_torrents_list_status_filters_match_cache_state() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "active-down", "Active Down", |t| {
        t.is_active = true;
        t.is_open = true;
        t.complete = false;
    });
    seed_torrent_with(&db, "complete-idle", "Complete Idle", |t| {
        t.complete = true;
    });

    let res = client
        .get(url(addr, "/api/v1/torrents?status=completed&sort=name"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["torrents"][0]["hash"], "complete-idle");

    let res = client
        .get(url(addr, "/api/v1/torrents?status=active&sort=name"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["torrents"][0]["hash"], "active-down");
}

#[tokio::test]
async fn native_delete_rejects_malformed_delete_files() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "delete-parse", "Delete Parse");

    let res = client
        .delete(url(
            addr,
            "/api/v1/torrents/delete-parse?delete_files=not-a-boolean",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

// --- Single torrent not found ---

#[tokio::test]
async fn native_torrent_get_not_found() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/v1/torrents/nonexistenthash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn native_torrent_update_save_path_validates_and_checks_existence() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "move-hash", "Move Me");

    let res = client
        .put(url(addr, "/api/v1/torrents/missing"))
        .json(&serde_json::json!({ "save_path": "/data/new" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .put(url(addr, "/api/v1/torrents/move-hash"))
        .json(&serde_json::json!({ "save_path": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .put(url(addr, "/api/v1/torrents/move-hash"))
        .json(&serde_json::json!({ "save_path": "/data/new" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
}

#[tokio::test]
async fn qb_set_location_updates_cache_for_known_torrents() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "location-hash", "Location");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setLocation"))
        .form(&[("hashes", "location-hash"), ("location", "/data/new")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/v1/torrents/location-hash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["directory"], "/data/new");
}

#[tokio::test]
async fn native_file_priority_validates_body_and_reports_rtorrent_failure() {
    let (addr, client) = spawn_server().await;

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/files"))
        .json(&serde_json::json!({ "files": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/files"))
        .json(&serde_json::json!({ "files": [{ "index": 0, "priority": 1 }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
}

#[tokio::test]
async fn native_tracker_patch_validates_body_and_reports_rtorrent_failure() {
    let (addr, client) = spawn_server().await;

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/trackers"))
        .json(&serde_json::json!({ "add": ["  "], "remove": [], "edit": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/trackers"))
        .json(&serde_json::json!({ "add": ["udp://tracker.example/announce"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);
}

#[tokio::test]
async fn qb_torrent_properties_from_cache() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "prop-hash", "Properties");

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/properties?hash=prop-hash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total_size"], 100);
    assert_eq!(body["share_ratio"], 0.0);

    seed_torrent(&db, "ABCDEF1234567890", "Uppercase Hash");
    let res = client
        .get(url(
            addr,
            "/api/qb/v2/torrents/properties?hash=abcdef1234567890",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total_size"], 100);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/properties"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn qb_hash_mutations_are_case_insensitive() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    let uppercase = "ABCDEF1234567890";
    let lowercase = uppercase.to_ascii_lowercase();
    seed_torrent(&db, uppercase, "Case-insensitive");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setCategory"))
        .form(&[("hashes", lowercase.as_str()), ("category", "Movies")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTags"))
        .form(&[("hashes", lowercase.as_str()), ("tags", "tracked")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, &format!("/api/v1/torrents/{uppercase}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["category"], "Movies");
    assert_eq!(body["tags"], "tracked");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/delete"))
        .form(&[("hashes", lowercase.as_str()), ("deleteFiles", "false")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(!db.exists(uppercase).unwrap());
}

// --- qBit createCategory / removeCategories ---

#[tokio::test]
async fn qb_create_remove_category() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .form(&[("category", " TV "), ("savePath", "/data/tv")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/categories"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["TV"]["savePath"], "/data/tv");
    assert!(body.get(" TV ").is_none());

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/editCategory"))
        .form(&[("category", "   "), ("savePath", "/data/blank")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/removeCategories"))
        .form(&[("categories", "TV")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/categories"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("TV").is_none());
}

// --- qBit createTags / deleteTags ---

#[tokio::test]
async fn qb_create_delete_tags() {
    let (addr, client, _) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/createTags"))
        .form(&[("tags", "hd,remux")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/tags"))
        .send()
        .await
        .unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(body.contains(&"hd".to_string()));
    assert!(body.contains(&"remux".to_string()));

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/deleteTags"))
        .form(&[("tags", "hd")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/tags"))
        .send()
        .await
        .unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(!body.contains(&"hd".to_string()));
    assert!(body.contains(&"remux".to_string()));
}

// --- Sync maindata (empty) ---

#[tokio::test]
async fn qb_sync_maindata_empty() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], true);
    assert!(body["torrents"].as_object().unwrap().is_empty());
    assert!(body["categories"].as_object().unwrap().is_empty());
    assert!(body["tags"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn qb_sync_maindata_rejects_negative_revision() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn qb_sync_maindata_includes_category_and_tag_metadata() {
    let (addr, client, db) = spawn_server_with_db().await;
    db.upsert_category("Movies", "/data/movies").unwrap();
    db.ensure_tag("remux").unwrap();

    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], true);
    assert_eq!(body["categories"]["Movies"]["name"], "Movies");
    assert_eq!(body["categories"]["Movies"]["savePath"], "/data/movies");
    assert_eq!(body["tags"], serde_json::json!(["remux"]));
    let rid = body["rid"].as_i64().unwrap();

    let res = client
        .get(url(addr, &format!("/api/qb/v2/sync/maindata?rid={rid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    assert_eq!(body["categories"]["Movies"]["savePath"], "/data/movies");
    assert_eq!(body["tags"], serde_json::json!(["remux"]));
}

// --- Transfer info ---

#[tokio::test]
async fn qb_transfer_info() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/qb/v2/transfer/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["connection_status"], "unreachable");
}

// --- Health ---

#[tokio::test]
async fn health_unreachable_rtorrent() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/health")).send().await.unwrap();
    // rTorrent is unreachable → 503, but the endpoint itself works
    assert!(res.status() == 200 || res.status() == 503);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("status").is_some());
    assert_eq!(body["rtorrent"], "unreachable");
}

#[tokio::test]
async fn native_storage_reports_configured_roots() {
    let mut cfg = Config::test_default();
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("definitely-not-real");
    cfg.storage_roots = vec![root.path().to_path_buf(), missing.clone()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let res = client
        .get(url(addr, "/api/v1/storage"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let roots = body["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0]["path"], root.path().display().to_string());
    #[cfg(unix)]
    {
        assert_eq!(roots[0]["ok"], true);
        assert!(roots[0]["total_bytes"].as_u64().unwrap() > 0);
    }
    #[cfg(not(unix))]
    {
        assert_eq!(roots[0]["ok"], false);
        assert!(roots[0]["error"].as_str().unwrap().contains("unsupported"));
    }
    assert_eq!(roots[1]["ok"], false);
}

#[tokio::test]
async fn native_jobs_returns_empty_list_for_sidecar_mode() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/v1/jobs")).send().await.unwrap();
    assert_eq!(res.status(), 501);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "NOT_IMPLEMENTED");
}

#[tokio::test]
async fn native_session_features_validate_request_body() {
    let (addr, client) = spawn_server().await;
    let res = client
        .patch(url(addr, "/api/v1/session/features"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn native_session_features_can_be_read_and_put() {
    let (addr, client) = spawn_server().await;
    let res = client
        .get(url(addr, "/api/v1/session/features"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("dht").is_some());
    assert!(body.get("pex").is_some());

    let res = client
        .put(url(addr, "/api/v1/session/features"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn native_session_features_report_rtorrent_failure() {
    let (addr, client) = spawn_server().await;
    let res = client
        .patch(url(addr, "/api/v1/session/features"))
        .json(&serde_json::json!({ "dht": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn native_tracker_health_aggregates_cached_tracker_state() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "tracker-ok", "Tracker OK", |t| {
        t.tracker_url = "udp://tracker.example/announce".into();
        t.is_active = true;
        t.complete = true;
        t.peers_complete = 10;
        t.peers_connected = 2;
    });
    seed_torrent_with(&db, "tracker-error", "Tracker Error", |t| {
        t.tracker_url = "udp://tracker.example/announce".into();
        t.message = "timeout".into();
        t.peers_complete = 1;
        t.peers_connected = 3;
    });
    seed_torrent_with(&db, "other-tracker", "Other Tracker", |t| {
        t.tracker_url = "https://other.example/announce".into();
    });

    let res = client
        .get(url(addr, "/api/v1/tracker-health"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let trackers = body["trackers"].as_array().unwrap();
    assert_eq!(trackers.len(), 2);
    let main = trackers
        .iter()
        .find(|row| row["tracker"] == "udp://tracker.example/announce")
        .unwrap();
    assert_eq!(main["torrent_count"], 2);
    assert_eq!(main["active_count"], 1);
    assert_eq!(main["complete_count"], 1);
    assert_eq!(main["error_count"], 1);
    assert_eq!(main["seed_count"], 11);
    assert_eq!(main["peer_count"], 5);
}

#[tokio::test]
async fn native_engine_diagnostics_degrade_when_rtorrent_unreachable() {
    let (addr, client, _) = spawn_server_with_db().await;
    let res = client
        .get(url(addr, "/api/v1/engine"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body["provenance"]["sidecar_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(body["capabilities"].as_array().unwrap().len() >= 8);
    assert_eq!(body["http"]["user_agent"]["ok"], false);
    assert!(body["drift"].as_array().unwrap().len() >= 8);
    assert!(body["drift"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "unavailable"));
}

#[tokio::test]
async fn native_engine_command_index_degrades_when_rtorrent_unreachable() {
    let (addr, client, _) = spawn_server_with_db().await;
    let res = client
        .get(url(addr, "/api/v1/engine/commands"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["count"], 0);
    assert!(body["commands"].as_array().unwrap().is_empty());
    assert!(body["error"].as_str().unwrap().contains("XMLRPC"));
}

#[tokio::test]
async fn native_ratio_groups_round_trip_and_validate() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&serde_json::json!({
            "name": " ",
            "ratio_limit": 1.0,
            "seeding_time_limit": -1,
            "category": null,
            "tracker": null,
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let group = serde_json::json!({
        "name": "Archive",
        "ratio_limit": 2.5,
        "seeding_time_limit": 1440,
        "category": "Movies",
        "tracker": "tracker.example",
        "enabled": true
    });
    let res = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&group)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body[0]["name"], "Archive");
    assert_eq!(body[0]["ratio_limit"], 2.5);

    let res = client
        .get(url(addr, "/api/v1/ratio-groups"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);

    let res = client
        .delete(url(addr, "/api/v1/ratio-groups/Archive"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn native_ratio_group_apply_dry_run_matches_filters() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "ratio-match", "Ratio Match", |t| {
        t.category = "Movies".into();
        t.tracker_url = "udp://tracker.example/announce".into();
    });
    seed_torrent_with(&db, "ratio-category-only", "Ratio Category Only", |t| {
        t.category = "Movies".into();
        t.tracker_url = "udp://other.example/announce".into();
    });
    seed_torrent_with(&db, "ratio-tracker-only", "Ratio Tracker Only", |t| {
        t.category = "TV".into();
        t.tracker_url = "udp://tracker.example/announce".into();
    });

    let res = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&serde_json::json!({
            "name": "MoviesTracker",
            "ratio_limit": 2.0,
            "seeding_time_limit": 1440,
            "category": "Movies",
            "tracker": "tracker.example",
            "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/v1/ratio-groups/MoviesTracker"))
        .json(&serde_json::json!({ "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"], serde_json::json!(["ratio-match"]));

    let res = client
        .post(url(addr, "/api/v1/ratio-groups/Missing"))
        .json(&serde_json::json!({ "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn native_workflows_round_trip_and_validate() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "",
            "enabled": true,
            "event": "completed",
            "action": "webhook",
            "category": null,
            "tracker": null,
            "command": null,
            "url": "https://example.invalid/hook",
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Notify",
            "enabled": true,
            "event": "completed",
            "action": "webhook",
            "category": "Movies",
            "tracker": null,
            "command": null,
            "url": "https://example.invalid/hook",
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert_eq!(body[0]["name"], "Notify");

    let res = client
        .get(url(addr, "/api/v1/workflows"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);

    let res = client
        .delete(url(addr, &format!("/api/v1/workflows/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn native_rss_rules_round_trip_validate_and_match() {
    let (addr, client, _, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
    let mut rx = tx.subscribe();

    let res = client
        .post(url(addr, "/api/v1/rss-rules"))
        .json(&serde_json::json!({
            "id": "",
            "name": "",
            "enabled": true,
            "feed_url": "https://example.invalid/rss",
            "include": "linux",
            "exclude": null,
            "category": null,
            "save_path": null,
            "tags": [],
            "start": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/rss-rules"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Linux ISOs",
            "enabled": true,
            "feed_url": "https://example.invalid/rss",
            "include": "ubuntu, fedora",
            "exclude": "cam",
            "category": "isos",
            "save_path": "/data/isos",
            "tags": ["rss", "linux"],
            "start": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert_eq!(body[0]["tags"], serde_json::json!(["rss", "linux"]));
    assert_event(&mut rx, "rss_rules_updated").await;

    let res = client
        .post(url(addr, "/api/v1/rss-rules/test"))
        .json(&serde_json::json!({
            "title": "Ubuntu 26.04 Server ISO",
            "link": "https://example.invalid/ubuntu"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["matches"][0]["matched"], true);
    assert_eq!(body["matches"][0]["category"], "isos");
    assert_eq!(body["matches"][0]["start"], false);

    let res = client
        .post(url(addr, "/api/v1/rss-rules/apply"))
        .json(&serde_json::json!({
            "title": "Ubuntu 26.04 Server ISO",
            "link": "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"], serde_json::json!(["Linux ISOs"]));

    let res = client
        .post(url(addr, "/api/v1/rss-rules/apply"))
        .json(&serde_json::json!({
            "title": "Ubuntu 26.04 Server ISO",
            "link": "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
            "dry_run": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], false);
    assert!(body["applied"].as_array().unwrap().is_empty());
    assert_eq!(body["errors"].as_array().unwrap().len(), 1);

    let res = client
        .post(url(addr, "/api/v1/rss-rules/test"))
        .json(&serde_json::json!({
            "title": "Ubuntu cam release",
            "link": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["matches"][0]["matched"], false);
    assert_eq!(body["matches"][0]["reason"], "exclude pattern matched");

    let res = client
        .delete(url(addr, &format!("/api/v1/rss-rules/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
    assert_event(&mut rx, "rss_rules_updated").await;
}

#[tokio::test]
async fn native_saved_views_round_trip_and_emit_events() {
    let (addr, client, _, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
    let mut rx = tx.subscribe();

    let res = client
        .post(url(addr, "/api/v1/saved-views"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Movies",
            "params": {
                "filter": "1080p",
                "status": "completed",
                "category": "Movies",
                "tag": "archive",
                "sort": "ratio",
                "dir": "desc"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();
    assert!(!id.is_empty());
    assert_eq!(body[0]["params"]["category"], "Movies");
    assert_event(&mut rx, "saved_views_updated").await;

    let res = client
        .get(url(addr, "/api/v1/saved-views"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);

    let res = client
        .delete(url(addr, &format!("/api/v1/saved-views/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_array().unwrap().is_empty());
    assert_event(&mut rx, "saved_views_updated").await;
}

#[tokio::test]
async fn native_cross_seed_helper_validates_and_previews() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/v1/cross-seed"))
        .json(&serde_json::json!({
            "hashes": [],
            "trackers": ["udp://tracker.example/announce"],
            "reannounce": true,
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/cross-seed"))
        .json(&serde_json::json!({
            "hashes": ["cross-a", "cross-b"],
            "trackers": ["udp://tracker.example/announce"],
            "reannounce": true,
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"], serde_json::json!(["cross-a", "cross-b"]));
}

#[tokio::test]
async fn qb_rss_rules_use_native_rule_store() {
    let (addr, client, _, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
    let mut rx = tx.subscribe();

    let rule = serde_json::json!({
        "enabled": true,
        "affectedFeeds": ["https://example.invalid/rss"],
        "mustContain": "ubuntu",
        "mustNotContain": "cam",
        "assignedCategory": "isos",
        "savePath": "/data/isos",
        "addPaused": true,
        "tags": "rss,linux"
    });
    let res = client
        .post(url(addr, "/api/qb/v2/rss/setRule"))
        .form(&[("ruleName", "Linux ISOs"), ("rule", &rule.to_string())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "rss_rules_updated").await;

    let res = client
        .get(url(addr, "/api/qb/v2/rss/rules"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["Linux ISOs"]["assignedCategory"], "isos");
    assert_eq!(body["Linux ISOs"]["addPaused"], true);

    let res = client
        .get(url(
            addr,
            "/api/qb/v2/rss/matchingArticles?article=Ubuntu%2026.04%20Server",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body, serde_json::json!(["Linux ISOs"]));

    let res = client
        .post(url(addr, "/api/qb/v2/rss/renameRule"))
        .form(&[("ruleName", "Linux ISOs"), ("newRuleName", "Linux")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "rss_rules_updated").await;

    let res = client
        .post(url(addr, "/api/qb/v2/rss/removeRule"))
        .form(&[("ruleName", "Linux")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "rss_rules_updated").await;
}

#[tokio::test]
async fn qb_rss_rule_rejects_malformed_fields_instead_of_defaulting_them() {
    let (addr, client) = spawn_server().await;
    let malformed_rules = [
        serde_json::json!({}),
        serde_json::json!({
            "affectedFeeds": "https://example.invalid/rss",
            "mustContain": "ubuntu"
        }),
        serde_json::json!({
            "affectedFeeds": [
                "https://example.invalid/one",
                "https://example.invalid/two"
            ],
            "mustContain": "ubuntu"
        }),
        serde_json::json!({
            "affectedFeeds": ["https://example.invalid/rss"],
            "mustContain": "ubuntu",
            "enabled": "true"
        }),
        serde_json::json!({
            "affectedFeeds": ["https://example.invalid/rss"],
            "mustContain": 42
        }),
        serde_json::json!({
            "affectedFeeds": ["https://example.invalid/rss"],
            "mustContain": "ubuntu",
            "tags": ["linux"]
        }),
        serde_json::json!({
            "affectedFeeds": ["https://example.invalid/rss"],
            "mustContain": "ubuntu",
            "addPaused": "true"
        }),
    ];

    for rule in malformed_rules {
        let raw = rule.to_string();
        let response = client
            .post(url(addr, "/api/qb/v2/rss/setRule"))
            .form(&[("ruleName", "malformed"), ("rule", raw.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "rule={raw}");
    }

    let rules: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/rss/rules"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rules, serde_json::json!({}));
}

#[tokio::test]
async fn native_workflow_run_dry_run_matches_completed_filters() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "workflow-match", "Workflow Match", |t| {
        t.complete = true;
        t.category = "Movies".into();
        t.tracker_url = "udp://tracker.example/announce".into();
    });
    seed_torrent_with(&db, "workflow-incomplete", "Workflow Incomplete", |t| {
        t.complete = false;
        t.category = "Movies".into();
        t.tracker_url = "udp://tracker.example/announce".into();
    });

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Move complete movies",
            "enabled": true,
            "event": "completed",
            "action": "set_location",
            "category": "Movies",
            "tracker": "tracker.example",
            "command": null,
            "url": null,
            "target_path": "/data/archive"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();

    let res = client
        .post(url(addr, &format!("/api/v1/workflows/{id}")))
        .json(&serde_json::json!({ "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"], serde_json::json!(["workflow-match"]));

    let res = client
        .get(url(addr, "/api/v1/workflow-runs"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["rule_id"], id);
    assert_eq!(body[0]["rule_name"], "Move complete movies");
    assert_eq!(body[0]["dry_run"], true);
    assert_eq!(body[0]["matched"], serde_json::json!(["workflow-match"]));
    assert_eq!(body[0]["applied"], serde_json::json!(["workflow-match"]));
    assert!(body[0]["errors"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn native_workflow_webhook_executes_and_records_history() {
    let (hook_addr, mut hook_rx) = spawn_webhook_receiver().await;
    let mut cfg = Config::test_default();
    cfg.workflows.allow_private_webhooks = true;
    let (addr, client, db) = spawn_server_with_config(cfg).await;
    seed_torrent_with(&db, "workflow-webhook", "Workflow Webhook", |t| {
        t.complete = true;
        t.category = "Movies".into();
    });

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Notify complete movie",
            "enabled": true,
            "event": "completed",
            "action": "webhook",
            "category": "Movies",
            "tracker": null,
            "command": null,
            "url": format!("http://{hook_addr}/hook"),
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();

    let res = client
        .post(url(addr, &format!("/api/v1/workflows/{id}")))
        .json(&serde_json::json!({ "dry_run": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], false);
    assert_eq!(body["applied"], serde_json::json!(["workflow-webhook"]));
    assert!(body["errors"].as_array().unwrap().is_empty());

    let payload = tokio::time::timeout(std::time::Duration::from_secs(2), hook_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(payload["workflow_id"], id);
    assert_eq!(payload["workflow_name"], "Notify complete movie");
    assert_eq!(payload["event"], "completed");
    assert_eq!(payload["action"], "webhook");
    assert_eq!(payload["hash"], "workflow-webhook");
    assert_eq!(payload["category"], "Movies");

    let res = client
        .get(url(addr, "/api/v1/workflow-runs"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body[0]["rule_id"], id);
    assert_eq!(body[0]["dry_run"], false);
    assert_eq!(body[0]["matched"], serde_json::json!(["workflow-webhook"]));
    assert_eq!(body[0]["applied"], serde_json::json!(["workflow-webhook"]));
}

#[tokio::test]
async fn native_workflow_script_execution_requires_config_gate() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent_with(&db, "script-match", "Script Match", |t| {
        t.complete = true;
    });

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Script",
            "enabled": true,
            "event": "completed",
            "action": "script",
            "category": null,
            "tracker": null,
            "command": "/bin/true",
            "url": null,
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();

    let res = client
        .post(url(addr, &format!("/api/v1/workflows/{id}")))
        .json(&serde_json::json!({ "dry_run": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body["applied"].as_array().unwrap().is_empty());
    assert!(body["errors"][0]
        .as_str()
        .unwrap()
        .contains("script execution is not enabled"));
}

#[tokio::test]
async fn native_workflow_script_execution_runs_when_enabled() {
    let mut cfg = Config::test_default();
    cfg.workflows.allow_scripts = true;
    #[cfg(windows)]
    let (script_dir, command) = (
        std::path::PathBuf::from(r"C:\Windows\System32"),
        r"C:\Windows\System32\cmd.exe /C exit 0",
    );
    #[cfg(not(windows))]
    let (script_dir, command) = (std::path::PathBuf::from("/bin"), "/bin/true");
    cfg.workflows.allowed_script_dirs = vec![script_dir];
    let (addr, client, db) = spawn_server_with_config(cfg).await;
    seed_torrent_with(&db, "script-run", "Script Run", |t| {
        t.complete = true;
    });

    let res = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "",
            "name": "Script",
            "enabled": true,
            "event": "completed",
            "action": "script",
            "category": null,
            "tracker": null,
            "command": command,
            "url": null,
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let id = body[0]["id"].as_str().unwrap();

    let res = client
        .post(url(addr, &format!("/api/v1/workflows/{id}")))
        .json(&serde_json::json!({ "dry_run": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["applied"], serde_json::json!(["script-run"]));
    assert!(body["errors"].as_array().unwrap().is_empty());
}

// --- Sync maindata incremental (rid) ---

#[tokio::test]
async fn qb_sync_maindata_incremental() {
    let (addr, client) = spawn_server().await;

    // Full update (rid=0) sets a rid in response
    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], true);
    let rid = body["rid"].as_i64().unwrap();

    // Subsequent call with that rid → incremental, full_update=false
    let res = client
        .get(url(addr, &format!("/api/qb/v2/sync/maindata?rid={rid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    assert_eq!(body["rid"].as_i64().unwrap(), rid);
    // No changes → torrents delta is empty
    assert!(body["torrents"].as_object().unwrap().is_empty());
}

#[tokio::test]
async fn qb_sync_maindata_incremental_includes_removed_torrents() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), Arc::new(SuccessfulBackend)).await;
    seed_torrent(&db, "removed-hash", "Removed");

    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let rid = body["rid"].as_i64().unwrap();

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/delete"))
        .form(&[("hashes", "removed-hash"), ("deleteFiles", "false")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, &format!("/api/qb/v2/sync/maindata?rid={rid}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    assert_eq!(body["torrents"].as_object().unwrap().len(), 0);
    assert_eq!(
        body["torrents_removed"],
        serde_json::json!(["removed-hash"])
    );
    assert!(body["rid"].as_i64().unwrap() > rid);
}

#[tokio::test]
async fn add_torrent_rejects_empty_payloads() {
    let (addr, client) = spawn_server().await;

    let form = reqwest::multipart::Form::new().text("magnet", "   ");
    let res = client
        .post(url(addr, "/api/v1/torrents"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let form = reqwest::multipart::Form::new().text("urls", "\n  \n");
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    assert_eq!(res.text().await.unwrap(), "Fails.");
}

// --- Bulk actions (empty hash list → OK with no-op) ---

#[tokio::test]
async fn bulk_dry_run() {
    let (addr, client) = spawn_server().await;
    let res = client
        .post(url(addr, "/api/v1/bulk/stop"))
        .json(&serde_json::json!({ "hashes": ["abc123", "def456"], "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn bulk_stop_processes_every_hash_exactly_once_concurrently() {
    // bulk_action() runs one task per hash under a bounded semaphore instead
    // of a sequential loop -- this guards against a concurrency bug losing
    // or duplicating results. The backend here points at a nonexistent
    // socket, so every call fails, but every one of the 200 hashes must
    // still show up exactly once, in applied+errors combined.
    let (addr, client) = spawn_server().await;
    let hashes: Vec<String> = (0..200).map(|i| format!("hash{i:04}")).collect();
    let res = client
        .post(url(addr, "/api/v1/bulk/stop"))
        .json(&serde_json::json!({ "hashes": hashes, "dry_run": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let applied = body["applied"].as_array().unwrap();
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(applied.len() + errors.len(), 200);

    let mut seen: std::collections::HashSet<String> = applied
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    for e in errors {
        let msg = e.as_str().unwrap();
        let hash = msg.split(':').next().unwrap().to_owned();
        seen.insert(hash);
    }
    assert_eq!(seen.len(), 200, "every hash must appear exactly once");
}

#[tokio::test]
async fn bulk_unknown_action_rejected() {
    let (addr, client) = spawn_server().await;
    let res = client
        .post(url(addr, "/api/v1/bulk/not-a-real-action"))
        .json(&serde_json::json!({ "hashes": ["abc123"], "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn bulk_category_and_location_validate_and_preview() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/v1/bulk/set-category"))
        .json(&serde_json::json!({ "hashes": ["abc"], "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/bulk/set-location"))
        .json(&serde_json::json!({ "hashes": ["abc"], "save_path": " ", "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/bulk/set-category"))
        .json(&serde_json::json!({
            "hashes": ["abc", "def"],
            "category": "Movies",
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"].as_array().unwrap().len(), 2);

    let res = client
        .post(url(addr, "/api/v1/bulk/set-location"))
        .json(&serde_json::json!({
            "hashes": ["abc"],
            "save_path": "/data/new",
            "dry_run": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn bulk_set_category_applies_to_cached_torrents() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "bulk-cat-hash", "Bulk Category");

    let res = client
        .post(url(addr, "/api/v1/bulk/set-category"))
        .json(&serde_json::json!({
            "hashes": ["bulk-cat-hash"],
            "category": "Movies",
            "dry_run": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body["errors"].as_array().unwrap().len() <= 1);

    let res = client
        .get(url(addr, "/api/v1/torrents/bulk-cat-hash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["category"], "Movies");
}

#[tokio::test]
async fn qb_extended_torrent_forms_parse() {
    let (addr, client) = spawn_server().await;

    let form = reqwest::multipart::Form::new()
        .text(
            "urls",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        )
        .text("savepath", "/data/incoming")
        .text("category", "sonarr")
        .text("tags", "tv,import")
        .text("paused", "false")
        .text("stopped", "true")
        .text("skip_checking", "true")
        .text("contentLayout", "Original")
        .text("autoTMM", "false")
        .text("ratioLimit", "2")
        .text("seedingTimeLimit", "1440");
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 501);
    assert_eq!(res.text().await.unwrap(), "Fails.");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTrackers"))
        .form(&[("hashes", ""), ("urls", "udp://tracker.example/announce")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setShareLimits"))
        .form(&[
            ("hashes", ""),
            ("ratioLimit", "1.5"),
            ("seedingTimeLimit", "3600"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/editTracker"))
        .form(&[("hash", "abc")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/removeTrackers"))
        .form(&[("hash", "abc"), ("urls", "")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/toggleSequentialDownload"))
        .form(&[("hashes", "")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 501);
}

#[tokio::test]
async fn qb_hashes_all_expands_from_cache() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "hash-a", "Alpha");
    seed_torrent(&db, "hash-b", "Beta");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setCategory"))
        .form(&[("hashes", "all"), ("category", "Movies")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/v1/torrents?sort=name"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let torrents = body["torrents"].as_array().unwrap();
    assert_eq!(torrents.len(), 2);
    assert!(torrents.iter().all(|t| t["category"] == "Movies"));
}

#[tokio::test]
async fn qb_maindata_delta_includes_metadata_changes() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "meta-hash", "Metadata");
    seed_torrent(&db, "tag-delta-hash", "Tag Delta");

    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=0"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let initial_rid = body["rid"].as_i64().unwrap();

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setCategory"))
        .form(&[("hashes", "meta-hash"), ("category", "Movies")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/sync/maindata?rid={initial_rid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    assert_eq!(body["torrents"]["meta-hash"]["category"], "Movies");
    let rid_after_category = body["rid"].as_i64().unwrap();
    assert!(rid_after_category > 1);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setCategory"))
        .form(&[("hashes", "meta-hash"), ("category", "Shows")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/sync/maindata?rid={rid_after_category}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    assert_eq!(body["torrents"]["meta-hash"]["category"], "Shows");
    assert!(body["rid"].as_i64().unwrap() > rid_after_category);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/removeCategories"))
        .form(&[("categories", "Shows")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/sync/maindata?rid={initial_rid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["torrents"]["meta-hash"]["category"], "");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTags"))
        .form(&[("hashes", "tag-delta-hash"), ("tags", "gone")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/deleteTags"))
        .form(&[("tags", "gone")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/sync/maindata?rid={initial_rid}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["torrents"]["tag-delta-hash"]["tags"], "");
}

#[tokio::test]
async fn qb_set_tags_replaces_cache_tags() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "tag-hash", "Tagged");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTags"))
        .form(&[("hashes", "tag-hash"), ("tags", "old,keep")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setTags"))
        .form(&[("hashes", "tag-hash"), ("tags", "new,keep")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/v1/torrents/tag-hash"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["tags"], "keep,new");
}

#[tokio::test]
async fn qb_inert_surfaces_are_compatible() {
    let (addr, client, db, _) = spawn_server_with_config_and_events(Config::test_default()).await;
    // These are optional/projection-only surfaces, but they still require a
    // real torrent target. Using an unknown hash would turn a compatibility
    // test into an assertion that the API silently accepts typos.
    seed_torrent(&db, "abc", "Compatibility fixture");

    for path in [
        "/api/qb/v2/torrents/webseeds?hash=abc",
        "/api/qb/v2/torrents/pieceStates?hash=abc",
        "/api/qb/v2/torrents/pieceHashes?hash=abc",
        "/api/qb/v2/log/main",
        "/api/qb/v2/search/categories",
        "/api/qb/v2/search/plugins",
        "/api/qb/v2/rss/matchingArticles",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert!(body.as_array().is_some(), "{path}");
    }

    let res = client
        .get(url(addr, "/api/qb/v2/log/peers"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 501);

    let res = client
        .get(url(addr, "/api/qb/v2/search/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["status"], "Stopped");

    let res = client
        .post(url(addr, "/api/qb/v2/search/start"))
        .form(&[("pattern", "linux")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["id"], 1);

    for path in [
        "/api/qb/v2/transfer/speedLimitsMode",
        "/api/qb/v2/transfer/downloadLimit",
        "/api/qb/v2/transfer/uploadLimit",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 501, "{path}");
    }
}

#[tokio::test]
async fn qb_mutation_booleans_fail_closed_and_capabilities_do_not_overclaim() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/delete"))
        .form(&[("hashes", "all"), ("deleteFiles", "not-a-boolean")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // The rTorrent adapter inherits rejecting implementations for mode
    // mutations. Its capability manifest must therefore fail closed before
    // the request reaches the unreachable stub client.
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setForceStart"))
        .form(&[("hashes", "all"), ("value", "true")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setForceStart"))
        .form(&[("hashes", "all")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn qb_limit_mutations_require_explicit_valid_values() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "limit-parse", "Limit Parse");

    for (path, form) in [
        (
            "/api/qb/v2/torrents/setDownloadLimit",
            vec![("hashes", "limit-parse")],
        ),
        (
            "/api/qb/v2/torrents/setUploadLimit",
            vec![("hashes", "limit-parse"), ("limit", "-1")],
        ),
        ("/api/qb/v2/transfer/setDownloadLimit", vec![]),
    ] {
        let res = client
            .post(url(addr, path))
            .form(&form)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400, "{path}");
    }

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setShareLimits"))
        .form(&[("hashes", "limit-parse")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setShareLimits"))
        .form(&[("hashes", "limit-parse"), ("ratioLimit", "-1")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setShareLimits"))
        .form(&[("hashes", "limit-parse"), ("seedingTimeLimit", "-3")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn qb_torrent_export_streams_rtorrent_session_blob() {
    let session = tempfile::tempdir().unwrap();
    std::env::set_var("TNG_SESSION_DIR", session.path());
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "ABCDEF", "Exported");
    let raw = b"d4:infod4:name8:exportedee".to_vec();
    std::fs::write(session.path().join("ABCDEF.torrent"), &raw).unwrap();

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/export?hash=ABCDEF"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/x-bittorrent"
    );
    assert_eq!(res.bytes().await.unwrap().as_ref(), raw.as_slice());
    std::env::remove_var("TNG_SESSION_DIR");
}

#[tokio::test]
async fn qb_search_plugins_jobs_and_rss_items_are_stateful() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/qb/v2/search/installPlugin"))
        .form(&[("sources", "https://example.test/plugins/linux.py")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let plugins: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/plugins"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plugins.as_array().unwrap()[0]["name"], "linux.py");

    let categories: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/categories"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(categories, serde_json::json!(["all"]));

    let res = client
        .post(url(addr, "/api/qb/v2/search/enablePlugin"))
        .form(&[("names", "linux.py"), ("enable", "false")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let plugins: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/plugins"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plugins.as_array().unwrap()[0]["enabled"], false);

    let categories: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/categories"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(categories, serde_json::json!([]));

    let job: serde_json::Value = client
        .post(url(addr, "/api/qb/v2/search/start"))
        .form(&[("pattern", "debian"), ("plugins", "linux.py")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(job["id"], 1);

    let results: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/results?id=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(results["pattern"], "debian");
    assert_eq!(results["plugins"], "linux.py");

    let res = client
        .post(url(addr, "/api/qb/v2/search/enablePlugin"))
        .form(&[("names", "linux.py"), ("enable", "not-a-boolean")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/search/stop"))
        .form(&[("id", "999")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .get(url(addr, "/api/qb/v2/search/results?id=999"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .get(url(
            addr,
            "/api/qb/v2/search/results?id=1&limit=not-a-number",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    for (endpoint, form) in [
        ("/api/qb/v2/rss/addFolder", vec![("path", "linux")]),
        (
            "/api/qb/v2/rss/addFeed",
            vec![
                ("url", "https://example.test/rss"),
                ("path", "linux/example"),
            ],
        ),
    ] {
        let res = client
            .post(url(addr, endpoint))
            .form(&form)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{endpoint}");
    }

    let items: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/rss/items"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(items["linux"]["type"], "folder");
    assert_eq!(items["linux/example"]["url"], "https://example.test/rss");

    let res = client
        .post(url(addr, "/api/qb/v2/rss/moveItem"))
        .form(&[("itemPath", "linux/example"), ("destPath", "linux/moved")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = client
        .post(url(addr, "/api/qb/v2/rss/markAsRead"))
        .form(&[("itemPath", "linux/moved")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = client
        .post(url(addr, "/api/qb/v2/rss/refreshItem"))
        .form(&[("itemPath", "linux/moved")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let items: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/rss/items"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(items.get("linux/example").is_none());
    assert_eq!(items["linux/moved"]["read"], true);
    assert!(items["linux/moved"]["lastBuildDate"].as_i64().unwrap() > 0);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/markAsRead"))
        .form(&[("itemPath", "linux/missing")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/refreshItem"))
        .form(&[("itemPath", "linux/missing")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/addFeed"))
        .form(&[
            ("url", "https://example.test/occupied"),
            ("path", "linux/occupied"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/moveItem"))
        .form(&[("itemPath", "linux/moved"), ("destPath", "linux/occupied")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 409);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/moveItem"))
        .form(&[("itemPath", "linux/missing"), ("destPath", "linux/new")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn qb_torrents_info_rejects_malformed_pagination_booleans() {
    let (addr, client) = spawn_server().await;

    for path in [
        "/api/qb/v2/torrents/info?limit=not-a-number",
        "/api/qb/v2/torrents/info?offset=-1",
        "/api/qb/v2/torrents/info?reverse=not-a-boolean",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 400, "{path}");
    }
}

#[tokio::test]
async fn qb_log_main_returns_retained_app_events() {
    let (addr, client, db) = spawn_server_with_db().await;
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_000,
            level: "warn".to_owned(),
            kind: "test".to_owned(),
            message: "operator-visible warning".to_owned(),
            payload: "{}".to_owned(),
        },
        16,
    )
    .unwrap();
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_001,
            level: "error".to_owned(),
            kind: "test".to_owned(),
            message: "operator-visible error".to_owned(),
            payload: "{}".to_owned(),
        },
        16,
    )
    .unwrap();

    let res = client
        .get(url(addr, "/api/qb/v2/log/main?limit=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "operator-visible error");
    assert_eq!(entries[0]["timestamp"], 1_700_000_001);
    assert_eq!(entries[0]["type"], 4);

    let res = client
        .get(url(addr, "/api/qb/v2/log/main?limit=1&warning=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "operator-visible warning");
    assert_eq!(entries[0]["type"], 2);

    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_002,
            level: "info".to_owned(),
            kind: "test".to_owned(),
            message: "newer info".to_owned(),
            payload: "{}".to_owned(),
        },
        16,
    )
    .unwrap();

    let res = client
        .get(url(addr, "/api/qb/v2/log/main?limit=1&warning=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "operator-visible warning");

    let res = client
        .get(url(addr, "/api/qb/v2/log/main?limit=10&last_known_id=2"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let entries = body.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["message"], "newer info");
    assert!(entries[0]["id"].as_i64().unwrap() > 2);
}

#[tokio::test]
async fn native_logs_returns_filtered_app_events() {
    let (addr, client, db) = spawn_server_with_db().await;
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_010,
            level: "info".to_owned(),
            kind: "sidecar_started".to_owned(),
            message: "started".to_owned(),
            payload: "{}".to_owned(),
        },
        16,
    )
    .unwrap();
    db.append_app_event(
        &AppEventRow {
            event_id: None,
            occurred_at: 1_700_000_011,
            level: "warn".to_owned(),
            kind: "rtorrent_log".to_owned(),
            message: "tracker warning".to_owned(),
            payload: serde_json::json!({"component":"rtorrent"}).to_string(),
        },
        16,
    )
    .unwrap();

    let res = client
        .get(url(addr, "/api/v1/logs?level=warn&kind=rtorrent_log"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "tracker warning");
    assert_eq!(logs[0]["kind"], "rtorrent_log");
    assert_eq!(logs[0]["level"], "warn");

    let res = client
        .get(url(addr, "/api/v1/logs?last_known_id=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    let logs = body["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["message"], "tracker warning");
    assert!(logs[0]["event_id"].as_i64().unwrap() > 1);
}

#[tokio::test]
async fn api_responses_echo_safe_request_id() {
    let (addr, client) = spawn_server().await;

    let res = client
        .get(url(addr, "/api/qb/v2/app/version"))
        .header("x-request-id", "arr-client-42.trace/7")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("arr-client-42.trace/7")
    );

    let res = client
        .get(url(addr, "/api/qb/v2/app/version"))
        .header("x-request-id", "bad value")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let generated = res
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(generated.starts_with("tng-"));
}

#[tokio::test]
async fn qb_set_preferences_validates_json() {
    let (addr, client, db) = spawn_server_with_db().await;

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[("json", r#"{"queueing_enabled":false}"#)])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[("json", r#"{"dht":true,"pex":false}"#)])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[("json", "{bad json")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[(
            "json",
            r#"{"network_http_user_agent":"TorrentNG-Test/1.0"}"#,
        )])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 503);

    let events = db.list_app_events(10).unwrap();
    assert_eq!(events[0].kind, "rtorrent_user_agent_error");
    assert_eq!(events[0].level, "warn");
    assert!(!events[0].payload.contains("TorrentNG-Test/1.0"));
}

#[tokio::test]
async fn native_user_agent_failures_are_durable_and_sanitized() {
    let (addr, client, db) = spawn_server_with_db().await;

    let res = client
        .put(url(addr, "/api/v1/settings/user-agent"))
        .json(&serde_json::json!({ "user_agent": "TorrentNG-Native-Test/1.0" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 500);

    let events = db.list_app_events(10).unwrap();
    assert_eq!(events[0].kind, "rtorrent_user_agent_error");
    assert_eq!(events[0].level, "warn");
    assert!(!events[0].payload.contains("TorrentNG-Native-Test/1.0"));
}
