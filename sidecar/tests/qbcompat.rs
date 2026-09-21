//! Integration tests for the qBittorrent compatibility layer.
//!
//! These tests spin up the full axum router against an in-memory SQLite DB.
//! They do NOT require a running rTorrent instance — endpoints that touch rTorrent
//! are skipped unless RTORRENT_SOCKET is set.

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use reqwest::Client;
use rusqlite::{params, Connection};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};

// Re-use internal modules via the binary crate root.
use torrentng::{
    api::{handlers::MAX_BULK_TORRENTS, server::AppState, ws::Event},
    backend::{
        BackendCapabilities, BackendStatus, BackendTransferLimits, BackendType, TorrentBackend,
    },
    cache::{AppEventRow, Db, RatioGroup, TorrentRow, WorkflowRule},
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
    let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let db = Arc::new(Db::open(db_path.as_ref()).unwrap());
    spawn_server_with_existing_db_and_events(cfg, rt, backend, db).await
}

async fn spawn_server_with_existing_db_and_events(
    cfg: Config,
    rt: Arc<torrentng::rtorrent::Client>,
    backend: Arc<dyn TorrentBackend>,
    db: Arc<Db>,
) -> (SocketAddr, Client, Arc<Db>, broadcast::Sender<Event>) {
    let cfg = Arc::new(cfg);
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
        login_attempt_limiter: torrentng::auth::LoginAttemptLimiter::default(),
        control_plane_write: Arc::new(tokio::sync::Mutex::new(())),
    };
    let app: Router = torrentng::api::server::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
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
struct SuccessfulBackend {
    add_mutations: Option<Arc<AtomicUsize>>,
    tag_mutations: Option<Arc<AtomicUsize>>,
    tracker_mutations: Option<Arc<AtomicUsize>>,
    peer_mutations: Option<Arc<AtomicUsize>>,
    rule_mutations: Option<Arc<AtomicUsize>>,
}

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
        if let Some(add_mutations) = &self.add_mutations {
            add_mutations.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn add_torrent(
        &self,
        _data: &[u8],
        _save_path: &str,
        _category: &str,
        _start: bool,
    ) -> anyhow::Result<()> {
        if let Some(add_mutations) = &self.add_mutations {
            add_mutations.fetch_add(1, Ordering::SeqCst);
        }
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
        if let Some(tracker_mutations) = &self.tracker_mutations {
            tracker_mutations.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn add_peers(&self, _hash: &str, _peers: &[std::net::SocketAddr]) -> anyhow::Result<()> {
        if let Some(peer_mutations) = &self.peer_mutations {
            peer_mutations.fetch_add(1, Ordering::SeqCst);
        }
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
        if let Some(rule_mutations) = &self.rule_mutations {
            rule_mutations.fetch_add(1, Ordering::SeqCst);
        }
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
        if let Some(rule_mutations) = &self.rule_mutations {
            rule_mutations.fetch_add(1, Ordering::SeqCst);
        }
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
        if let Some(tag_mutations) = &self.tag_mutations {
            tag_mutations.fetch_add(1, Ordering::SeqCst);
        }
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
    Arc::new(SuccessfulBackend {
        add_mutations: None,
        tag_mutations: None,
        tracker_mutations: None,
        peer_mutations: None,
        rule_mutations: None,
    })
}

fn successful_backend_with_tag_counter(counter: Arc<AtomicUsize>) -> Arc<dyn TorrentBackend> {
    Arc::new(SuccessfulBackend {
        add_mutations: None,
        tag_mutations: Some(counter),
        tracker_mutations: None,
        peer_mutations: None,
        rule_mutations: None,
    })
}

fn successful_backend_with_rule_counter(counter: Arc<AtomicUsize>) -> Arc<dyn TorrentBackend> {
    Arc::new(SuccessfulBackend {
        add_mutations: None,
        tag_mutations: None,
        tracker_mutations: None,
        peer_mutations: None,
        rule_mutations: Some(counter),
    })
}

fn successful_backend_with_add_counter(counter: Arc<AtomicUsize>) -> Arc<dyn TorrentBackend> {
    Arc::new(SuccessfulBackend {
        add_mutations: Some(counter),
        tag_mutations: None,
        tracker_mutations: None,
        peer_mutations: None,
        rule_mutations: None,
    })
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

#[tokio::test]
async fn ratio_group_uses_the_selected_backend_for_share_limits() {
    let mutations = Arc::new(AtomicUsize::new(0));
    let (addr, client, db) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_rule_counter(mutations.clone()),
    )
    .await;
    seed_torrent(&db, "ratio-target", "Ratio target");
    db.upsert_ratio_group(RatioGroup {
        name: "all".to_owned(),
        ratio_limit: 1.5,
        seeding_time_limit: 3_600,
        category: None,
        tracker: None,
        enabled: true,
    })
    .unwrap();

    let response = client
        .post(url(addr, "/api/v1/ratio-groups/all"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = response.json().await.unwrap();
    assert_eq!(result["applied"], serde_json::json!(["ratio-target"]));
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ratio_and_workflow_rule_fanout_is_rejected_before_backend_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("cache.sqlite");
    let db = Arc::new(Db::open(&db_path).unwrap());
    {
        let mut connection = Connection::open(&db_path).unwrap();
        let transaction = connection.transaction().unwrap();
        let mut insert = transaction
            .prepare("INSERT INTO torrents(hash, name) VALUES(?1, ?2)")
            .unwrap();
        for index in 0..=MAX_BULK_TORRENTS {
            let hash = format!("rule-target-{index:05}");
            let name = format!("Torrent {index:05}");
            insert.execute(params![hash, name]).unwrap();
        }
        drop(insert);
        transaction.commit().unwrap();
    }

    db.upsert_ratio_group(RatioGroup {
        name: "all".to_owned(),
        ratio_limit: 1.5,
        seeding_time_limit: 3_600,
        category: None,
        tracker: None,
        enabled: true,
    })
    .unwrap();
    db.upsert_workflow_rule(WorkflowRule {
        id: "all".to_owned(),
        name: "All torrents".to_owned(),
        enabled: true,
        event: "added".to_owned(),
        action: "set_category".to_owned(),
        category: Some("reviewed".to_owned()),
        target_category: None,
        tracker: None,
        command: None,
        url: None,
        target_path: None,
    })
    .unwrap();

    let mutations = Arc::new(AtomicUsize::new(0));
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let (addr, client, _, _) = spawn_server_with_existing_db_and_events(
        Config::test_default(),
        rt,
        successful_backend_with_rule_counter(mutations.clone()),
        db.clone(),
    )
    .await;

    for path in ["/api/v1/ratio-groups/all", "/api/v1/workflows/all"] {
        let response = client
            .post(url(addr, path))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{path}");
    }
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    assert!(db.list_workflow_runs().unwrap().is_empty());
}

#[tokio::test]
async fn ratio_and_workflow_rule_stores_reject_growth_and_oversized_fields() {
    const MAX_STORED_RULES: usize = 1_024;
    let (addr, client, db) = spawn_server_with_db().await;
    let ratio_groups = (0..MAX_STORED_RULES)
        .map(|index| RatioGroup {
            name: format!("group-{index}"),
            ratio_limit: 1.0,
            seeding_time_limit: -1,
            category: None,
            tracker: None,
            enabled: true,
        })
        .collect::<Vec<_>>();
    db.set_kv(
        "ratio_groups",
        &serde_json::to_string(&ratio_groups).unwrap(),
    )
    .unwrap();
    let workflow_rules = (0..MAX_STORED_RULES)
        .map(|index| WorkflowRule {
            id: format!("workflow-{index}"),
            name: format!("Workflow {index}"),
            enabled: true,
            event: "completed".to_owned(),
            action: "webhook".to_owned(),
            category: None,
            target_category: None,
            tracker: None,
            command: None,
            url: Some("https://example.invalid/hook".to_owned()),
            target_path: None,
        })
        .collect::<Vec<_>>();
    db.set_kv(
        "workflow_rules",
        &serde_json::to_string(&workflow_rules).unwrap(),
    )
    .unwrap();

    let ratio_payload = |name: &str| {
        serde_json::json!({
            "name": name,
            "ratio_limit": 1.5,
            "seeding_time_limit": -1,
            "category": null,
            "tracker": null,
            "enabled": true
        })
    };
    let response = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&ratio_payload("new-group"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&ratio_payload("group-0"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = client
        .post(url(addr, "/api/v1/ratio-groups"))
        .json(&ratio_payload(&"x".repeat(257)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let workflow_payload = |id: &str| {
        serde_json::json!({
            "id": id,
            "name": "Capacity test",
            "enabled": true,
            "event": "completed",
            "action": "webhook",
            "category": null,
            "target_category": null,
            "tracker": null,
            "command": null,
            "url": "https://example.invalid/hook",
            "target_path": null
        })
    };
    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&workflow_payload("new-workflow"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&workflow_payload("workflow-0"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut oversized_workflow = workflow_payload("oversized-workflow");
    oversized_workflow["name"] = serde_json::json!("x".repeat(257));
    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&oversized_workflow)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert_eq!(db.list_ratio_groups().unwrap().len(), MAX_STORED_RULES);
    assert_eq!(db.list_workflow_rules().unwrap().len(), MAX_STORED_RULES);
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
async fn qb_login_throttles_repeated_failed_credentials_and_clears_on_success() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["login-rate-limit-token-20260921".to_owned()];
    let (addr, _, _) = spawn_server_with_config(cfg).await;
    let client = Client::new();

    for _ in 0..4 {
        let response = client
            .post(url(addr, "/api/qb/v2/auth/login"))
            .form(&[("username", "admin"), ("password", "wrong")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "Fails.");
    }

    let successful = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[
            ("username", "admin"),
            ("password", "login-rate-limit-token-20260921"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(successful.status(), StatusCode::OK);
    assert_eq!(successful.text().await.unwrap(), "Ok.");

    for _ in 0..10 {
        let response = client
            .post(url(addr, "/api/qb/v2/auth/login"))
            .form(&[("username", "admin"), ("password", "wrong")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "Fails.");
    }

    let limited = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn unauthenticated_cross_origin_login_is_rejected() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["login-csrf-test-token".to_owned()];
    cfg.auth.secret_key = Some("login-csrf-test-signing-key-32-bytes".to_owned());
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let cross_origin = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .header("Origin", "https://attacker.example.test")
        .header("Sec-Fetch-Site", "cross-site")
        .form(&[("username", "admin"), ("password", "login-csrf-test-token")])
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        cross_origin.headers().get_all("set-cookie").iter().count(),
        0
    );

    let same_origin = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .header("Origin", format!("http://{addr}"))
        .header("Sec-Fetch-Site", "same-origin")
        .form(&[("username", "admin"), ("password", "login-csrf-test-token")])
        .send()
        .await
        .unwrap();
    assert_eq!(same_origin.status(), StatusCode::OK);
    assert_eq!(
        same_origin.headers().get_all("set-cookie").iter().count(),
        2
    );
}

#[tokio::test]
async fn login_and_logout_mark_session_cookies_secure_by_default() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["secure-cookie-test-token".to_owned()];
    cfg.auth.secret_key = Some("secure-cookie-test-signing-key-32-bytes".to_owned());
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let login = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[
            ("username", "admin"),
            ("password", "secure-cookie-test-token"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let login_cookies = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(login_cookies.len(), 2);
    assert!(login_cookies
        .iter()
        .all(|cookie| cookie.contains("; Secure")));

    let logout = client
        .post(url(addr, "/api/qb/v2/auth/logout"))
        .header("Origin", format!("http://{addr}"))
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    let logout_cookies = logout
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(logout_cookies.len(), 2);
    assert!(logout_cookies
        .iter()
        .all(|cookie| cookie.contains("; Secure")));
}

#[tokio::test]
async fn loopback_http_can_explicitly_disable_secure_session_cookies() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["local-http-cookie-test-token".to_owned()];
    cfg.auth.secure_cookies = false;
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let login = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[
            ("username", "admin"),
            ("password", "local-http-cookie-test-token"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookies = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(cookies.len(), 2);
    assert!(cookies.iter().all(|cookie| !cookie.contains("; Secure")));
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
async fn invalid_legacy_session_cookie_does_not_shadow_valid_sid_cookie() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["cookie-precedence-test-token".to_owned()];
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let login = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[
            ("username", "admin"),
            ("password", "cookie-precedence-test-token"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let sid = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            value
                .to_str()
                .ok()?
                .strip_prefix("SID=")?
                .split(';')
                .next()
                .map(str::to_owned)
        })
        .expect("login should issue SID cookie");

    let response = Client::new()
        .get(url(addr, "/api/qb/v2/app/preferences"))
        .header("Cookie", format!("tng_session=stale; SID={sid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
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

    // Create via TorrentNG REST API
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

    // Delete via TorrentNG REST API
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
    let oversized_name = "x".repeat(257);

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "  ", "save_path": "/tmp" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": oversized_name, "save_path": "/tmp" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "Movies", "save_path": "x".repeat(4097) }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .delete(url(
            addr,
            &format!("/api/v1/categories/{}", "x".repeat(257)),
        ))
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
        .post(url(addr, "/api/v1/tags"))
        .json(&serde_json::json!({ "name": "x".repeat(257) }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .delete(url(addr, &format!("/api/v1/tags/{}", "x".repeat(257))))
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

    let res = client
        .put(url(addr, "/api/v1/torrents/hash/category"))
        .json(&serde_json::json!({ "category": "x".repeat(257) }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .post(url(addr, "/api/v1/torrents/hash/tags"))
        .json(&serde_json::json!({ "tags": vec!["tag"; 1025] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let res = client
        .delete(url(addr, "/api/v1/torrents/hash/tags"))
        .json(&serde_json::json!({ "tags": ["x".repeat(1024 * 1024)] }))
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
async fn native_torrent_actions_reject_hashes_missing_from_the_cache() {
    let (addr, client, _) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    let mut statuses = Vec::new();

    for path in [
        "/api/v1/torrents/missing/start",
        "/api/v1/torrents/missing/recheck",
        "/api/v1/torrents/missing/reannounce",
        "/api/v1/torrents/missing/trackers",
        "/api/v1/torrents/missing/files",
        "/api/v1/torrents/missing",
    ] {
        let request = if path.ends_with("/trackers") || path.ends_with("/files") {
            client.get(url(addr, path))
        } else if path.ends_with("/missing") {
            client.delete(url(addr, path))
        } else {
            client.post(url(addr, path))
        };
        statuses.push(request.send().await.unwrap().status());
    }

    let tracker_patch = client
        .patch(url(addr, "/api/v1/torrents/missing/trackers"))
        .json(&serde_json::json!({ "add": ["udp://tracker.example/announce"] }))
        .send()
        .await
        .unwrap();
    statuses.push(tracker_patch.status());

    let file_patch = client
        .patch(url(addr, "/api/v1/torrents/missing/files"))
        .json(&serde_json::json!({ "files": [{ "index": 0, "priority": 1 }] }))
        .send()
        .await
        .unwrap();
    statuses.push(file_patch.status());

    assert_eq!(statuses, vec![StatusCode::NOT_FOUND; 8]);

    for action in ["start", "stop", "recheck", "reannounce"] {
        let response = client
            .post(url(addr, &format!("/api/v1/bulk/{action}")))
            .json(&serde_json::json!({ "hashes": ["missing"] }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{action}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["applied"], serde_json::json!([]), "{action}");
        assert_eq!(
            body["errors"],
            serde_json::json!(["missing: not found"]),
            "{action}"
        );
    }
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
        t.state = 3;
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
async fn qb_nested_mutation_fanout_is_rejected_before_backend_calls() {
    let tracker_mutations = Arc::new(AtomicUsize::new(0));
    let peer_mutations = Arc::new(AtomicUsize::new(0));
    let tag_mutations = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(SuccessfulBackend {
        add_mutations: None,
        tag_mutations: Some(tag_mutations.clone()),
        tracker_mutations: Some(tracker_mutations.clone()),
        peer_mutations: Some(peer_mutations.clone()),
        rule_mutations: None,
    });
    let (addr, client, db) = spawn_server_with_backend(Config::test_default(), backend).await;
    let hashes = (0..11)
        .map(|index| format!("fanout-hash-{index}"))
        .collect::<Vec<_>>();
    for hash in &hashes {
        seed_torrent(&db, hash, hash);
    }
    let hash_selection = hashes.join("|");

    let trackers = (0..1_000)
        .map(|index| format!("udp://tracker.example/announce-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let response = client
        .post(url(addr, "/api/qb/v2/torrents/addTrackers"))
        .form(&[
            ("hashes", hash_selection.as_str()),
            ("urls", trackers.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(tracker_mutations.load(Ordering::SeqCst), 0);

    let tags = (0..1_000)
        .map(|index| format!("tag-{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let response = client
        .post(url(addr, "/api/qb/v2/torrents/addTags"))
        .form(&[("hashes", hash_selection.as_str()), ("tags", tags.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(tag_mutations.load(Ordering::SeqCst), 0);

    let peers = (0..1_000)
        .map(|index| format!("8.8.8.8:{}", 6_881 + index))
        .collect::<Vec<_>>()
        .join("|");
    let response = client
        .post(url(addr, "/api/qb/v2/torrents/addPeers"))
        .form(&[
            ("hashes", hash_selection.as_str()),
            ("peers", peers.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(peer_mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_live_stats_query_enforces_bounds_through_axum_extractor() {
    let (addr, client, db) = spawn_server_with_db().await;
    let hash = "a".repeat(40);
    seed_torrent(&db, &hash, "Live stats fixture");

    let response = client
        .get(url(addr, &format!("/api/v1/torrents/live?hashes={hash}")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let repeated = std::iter::repeat_n(hash.as_str(), 129)
        .collect::<Vec<_>>()
        .join(",");
    let response = client
        .get(url(
            addr,
            &format!("/api/v1/torrents/live?hashes={repeated}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let oversized = ",".repeat(8_321);
    let response = client
        .get(url(
            addr,
            &format!("/api/v1/torrents/live?hashes={oversized}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

    let res = client
        .get(url(addr, "/api/v1/torrents?status=%20COMPLETED%20"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["torrents"][0]["hash"], "complete-idle");

    let res = client
        .get(url(addr, "/api/v1/torrents?status=not-a-real-status"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 0);
    assert!(body["torrents"].as_array().unwrap().is_empty());
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
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "abc", "ABC");

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

    let too_many_files = (0..4_097)
        .map(|index| serde_json::json!({ "index": index, "priority": 1 }))
        .collect::<Vec<_>>();
    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/files"))
        .json(&serde_json::json!({ "files": too_many_files }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn native_tracker_patch_validates_body_and_reports_rtorrent_failure() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "abc", "ABC");

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/trackers"))
        .json(&serde_json::json!({ "add": ["  "], "remove": [], "edit": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let too_many_trackers = vec!["udp://tracker.example/announce"; 1_025];
    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/trackers"))
        .json(&serde_json::json!({ "add": too_many_trackers }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let res = client
        .patch(url(addr, "/api/v1/torrents/abc/trackers"))
        .json(&serde_json::json!({ "add": ["x".repeat(8_193)] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

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
async fn native_torrent_add_multipart_fields_are_bounded_and_unique() {
    let (addr, client, _) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;

    let oversized_category = reqwest::multipart::Form::new()
        .text(
            "magnet",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        )
        .text("category", "x".repeat(1024 * 1024));
    let res = client
        .post(url(addr, "/api/v1/torrents"))
        .multipart(oversized_category)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let duplicate_magnet = reqwest::multipart::Form::new()
        .text(
            "magnet",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        )
        .text(
            "magnet",
            "magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef01",
        );
    let res = client
        .post(url(addr, "/api/v1/torrents"))
        .multipart(duplicate_magnet)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let ambiguous_source = reqwest::multipart::Form::new()
        .text(
            "magnet",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        )
        .part(
            "torrent",
            reqwest::multipart::Part::bytes(b"not-empty".to_vec()),
        );
    let res = client
        .post(url(addr, "/api/v1/torrents"))
        .multipart(ambiguous_source)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn native_torrent_add_accepts_the_full_64_mib_payload_with_multipart_envelope() {
    const MAX_TORRENT_BYTES: usize = 64 * 1024 * 1024;
    let adds = Arc::new(AtomicUsize::new(0));
    let (addr, client, _) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_add_counter(adds.clone()),
    )
    .await;
    let form = reqwest::multipart::Form::new().part(
        "torrent",
        reqwest::multipart::Part::bytes(vec![b'x'; MAX_TORRENT_BYTES])
            .file_name("full-size.torrent"),
    );

    let response = client
        .post(url(addr, "/api/v1/torrents"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(adds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn qb_torrent_add_accepts_the_full_64_mib_payload_with_multipart_envelope() {
    const MAX_TORRENT_BYTES: usize = 64 * 1024 * 1024;
    let adds = Arc::new(AtomicUsize::new(0));
    let (addr, client, _) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_add_counter(adds.clone()),
    )
    .await;
    let form = reqwest::multipart::Form::new().part(
        "torrents",
        reqwest::multipart::Part::bytes(vec![b'x'; MAX_TORRENT_BYTES])
            .file_name("full-size.torrent"),
    );

    let response = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(adds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn larger_multipart_limit_does_not_raise_json_extractor_limit() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    let body = serde_json::json!({
        "id": "oversized",
        "name": "x".repeat(3 * 1024 * 1024),
        "enabled": true,
        "event": "added",
        "action": "set_category",
        "category": null,
        "target_category": null,
        "tracker": null,
        "command": null,
        "url": null,
        "target_path": null
    });

    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(db.list_workflow_rules().unwrap().is_empty());
}

#[tokio::test]
async fn qb_torrent_add_rejects_ambiguous_url_and_file_sources_before_backend_mutation() {
    let adds = Arc::new(AtomicUsize::new(0));
    let (addr, client, _) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_add_counter(adds.clone()),
    )
    .await;

    let url_first = reqwest::multipart::Form::new()
        .text(
            "urls",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        )
        .part(
            "torrents",
            reqwest::multipart::Part::bytes(b"torrent bytes".to_vec()),
        );
    let response = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(url_first)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(adds.load(Ordering::SeqCst), 0);

    let file_first = reqwest::multipart::Form::new()
        .part(
            "torrents",
            reqwest::multipart::Part::bytes(b"torrent bytes".to_vec()),
        )
        .text(
            "urls",
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        );
    let response = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(file_first)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(adds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cookie_and_proxy_authenticated_websockets_reject_cross_origin_handshakes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn websocket_status(
        addr: SocketAddr,
        host: &str,
        cookie: Option<&str>,
        origin: &str,
        proxy_user: Option<&str>,
    ) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let cookie_header =
            cookie.map_or_else(String::new, |cookie| format!("Cookie: {cookie}\r\n"));
        let proxy_user_header =
            proxy_user.map_or_else(String::new, |user| format!("X-Remote-User: {user}\r\n"));
        let same_origin = format!("http://{host}");
        let fetch_site = if origin.eq_ignore_ascii_case(&same_origin) {
            "same-origin"
        } else {
            "same-site"
        };
        let request = format!(
            "GET /ws HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nSec-Fetch-Site: {fetch_site}\r\n{cookie_header}{proxy_user_header}Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut buffer = [0_u8; 1024];
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "server closed before returning an HTTP response");
                response.extend_from_slice(&buffer[..count]);
            }
        })
        .await
        .expect("websocket handshake response timed out");
        String::from_utf8(response).unwrap()
    }

    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["websocket-test-token".to_owned()];
    cfg.auth.secret_key = Some("test-session-signing-key-32-bytes-long".to_owned());
    let (addr, client, _) = spawn_server_with_config(cfg).await;
    let login = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "websocket-test-token")])
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let cookie = value.strip_prefix("SID=")?.split(';').next()?;
            Some(format!("SID={cookie}"))
        })
        .expect("login should issue qBittorrent SID cookie");

    let host = format!("torrentng.example.test:{}", addr.port());
    let attacker_origin = format!("http://attacker.example.test:{}", addr.port());
    let response = websocket_status(addr, &host, Some(&cookie), &attacker_origin, None).await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");

    let response =
        websocket_status(addr, &host, Some(&cookie), &format!("http://{host}"), None).await;
    assert!(response.starts_with("HTTP/1.1 101"), "{response}");

    let (addr, _, _) = spawn_server_with_db().await;
    let host = format!("torrentng.example.test:{}", addr.port());
    let attacker_origin = format!("http://attacker.example.test:{}", addr.port());
    let response = websocket_status(addr, &host, None, &attacker_origin, None).await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    let response = websocket_status(addr, &host, None, &format!("http://{host}"), None).await;
    assert!(response.starts_with("HTTP/1.1 101"), "{response}");

    let mut proxy_cfg = Config::test_default();
    proxy_cfg.auth.api_tokens = vec!["proxy-websocket-test-token".to_owned()];
    proxy_cfg.auth.trust_proxy_header = true;
    let (proxy_addr, _, _) = spawn_server_with_config(proxy_cfg).await;
    let proxy_host = format!("torrentng.example.test:{}", proxy_addr.port());
    let attacker_origin = format!("http://attacker.example.test:{}", proxy_addr.port());
    let response = websocket_status(
        proxy_addr,
        &proxy_host,
        None,
        &attacker_origin,
        Some("alice"),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    let response = websocket_status(
        proxy_addr,
        &proxy_host,
        None,
        &format!("http://{proxy_host}"),
        Some("alice"),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 101"), "{response}");
}

#[tokio::test]
async fn trusted_proxy_identity_checks_browser_mutations() {
    let mut cfg = Config::test_default();
    cfg.auth.api_tokens = vec!["proxy-mutation-test-token".to_owned()];
    cfg.auth.trust_proxy_header = true;
    let (addr, client, _) = spawn_server_with_config(cfg).await;

    let cross_origin = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .header("X-Remote-User", "alice")
        .header("Origin", "https://attacker.example.test")
        .header("Sec-Fetch-Site", "cross-site")
        .form(&[("category", "forged"), ("savePath", "/tmp/forged")])
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);

    let same_origin = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .header("X-Remote-User", "alice")
        .header("Origin", format!("http://{addr}"))
        .header("Sec-Fetch-Site", "same-origin")
        .form(&[("category", "allowed"), ("savePath", "/tmp/allowed")])
        .send()
        .await
        .unwrap();
    assert_eq!(same_origin.status(), StatusCode::OK);

    let bearer = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .header("X-Remote-User", "alice")
        .bearer_auth("proxy-mutation-test-token")
        .form(&[("category", "bearer"), ("savePath", "/tmp/bearer")])
        .send()
        .await
        .unwrap();
    assert_eq!(bearer.status(), StatusCode::OK);
}

#[tokio::test]
async fn no_auth_mode_rejects_cross_origin_browser_mutations() {
    let (addr, client) = spawn_server().await;
    let cross_origin = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .header(
            "Origin",
            format!("http://attacker.example.test:{}", addr.port()),
        )
        .form(&[("category", "forged"), ("savePath", "/tmp/forged")])
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);

    let same_origin = client
        .post(url(addr, "/api/qb/v2/torrents/createCategory"))
        .header("Origin", format!("http://{addr}"))
        .form(&[("category", "allowed"), ("savePath", "/tmp/allowed")])
        .send()
        .await
        .unwrap();
    assert_eq!(same_origin.status(), StatusCode::OK);
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

#[tokio::test]
async fn tag_assignment_limit_is_rejected_before_backend_mutation() {
    let backend_calls = Arc::new(AtomicUsize::new(0));
    let (addr, client, db) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_tag_counter(backend_calls.clone()),
    )
    .await;
    seed_torrent(&db, "tag-capacity", "Tag capacity");

    let tags = (0..1_024)
        .map(|index| format!("tag-{index:04}"))
        .collect::<Vec<_>>();
    let tag_refs = tags.iter().map(String::as_str).collect::<Vec<_>>();
    db.add_torrent_tags("tag-capacity", &tag_refs).unwrap();

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTags"))
        .form(&[("hashes", "tag-capacity"), ("tags", "one-more")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    let res = client
        .post(url(addr, "/api/v1/torrents/tag-capacity/tags"))
        .json(&serde_json::json!({ "tags": ["one-more"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(backend_calls.load(Ordering::SeqCst), 0);
    assert_eq!(db.get_torrent_tags("tag-capacity").unwrap().len(), 1_024);
    assert!(!db.list_tags().unwrap().iter().any(|tag| tag == "one-more"));
}

#[tokio::test]
async fn qb_bulk_hash_selection_limit_is_checked_before_deduplication() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "same-hash", "Bounded selection");
    let repeated = std::iter::repeat_n("same-hash", 10_001)
        .collect::<Vec<_>>()
        .join("|");

    let response = client
        .post(url(addr, "/api/qb/v2/torrents/start"))
        .form(&[("hashes", repeated.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

    let oversized_name = "n".repeat(257);
    let res = client
        .post(url(addr, "/api/v1/rss-rules"))
        .json(&serde_json::json!({
            "id": "",
            "name": oversized_name,
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
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

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
async fn native_rss_sample_strings_are_bounded_by_json_extractors() {
    let (addr, client) = spawn_server().await;
    let oversized = "x".repeat(8_193);
    let cases = [
        (
            "/api/v1/rss-rules/test",
            serde_json::json!({ "title": oversized.clone(), "link": null }),
        ),
        (
            "/api/v1/rss-rules/test",
            serde_json::json!({ "title": "valid", "link": oversized.clone() }),
        ),
        (
            "/api/v1/rss-rules/apply",
            serde_json::json!({ "title": oversized.clone(), "link": "magnet:?xt=urn:btih:test" }),
        ),
        (
            "/api/v1/rss-rules/apply",
            serde_json::json!({ "title": "valid", "link": oversized }),
        ),
    ];

    for (path, body) in cases {
        let response = client
            .post(url(addr, path))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}"
        );
    }
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

    let hashes = (0..100)
        .map(|index| format!("cross-{index}"))
        .collect::<Vec<_>>();
    let trackers = (0..101)
        .map(|index| format!("udp://tracker-{index}.example/announce"))
        .collect::<Vec<_>>();
    let res = client
        .post(url(addr, "/api/v1/cross-seed"))
        .json(&serde_json::json!({ "hashes": hashes, "trackers": trackers }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn qb_rss_rules_use_native_rule_store() {
    let (addr, client, db, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
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
    let original_id = db.list_rss_rules().unwrap()[0].id.clone();

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

    let updated_rule = serde_json::json!({
        "enabled": true,
        "affectedFeeds": ["https://example.invalid/rss"],
        "mustContain": "debian",
        "mustNotContain": "cam",
        "assignedCategory": "isos",
        "savePath": "/data/isos",
        "addPaused": true,
        "tags": "rss,linux"
    });
    let res = client
        .post(url(addr, "/api/qb/v2/rss/setRule"))
        .form(&[
            ("ruleName", "Linux ISOs"),
            ("rule", &updated_rule.to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_event(&mut rx, "rss_rules_updated").await;
    let rules = db.list_rss_rules().unwrap();
    assert_eq!(rules.len(), 1, "setRule updates by name, not random id");
    assert_eq!(rules[0].id, original_id);
    assert_eq!(rules[0].include, "debian");

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
async fn native_workflow_set_category_separates_filter_from_target() {
    let mutations = Arc::new(AtomicUsize::new(0));
    let (addr, client, db) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_rule_counter(mutations.clone()),
    )
    .await;
    seed_torrent_with(&db, "category-match", "Category match", |torrent| {
        torrent.category = "Source".to_owned();
    });
    seed_torrent_with(&db, "category-other", "Other category", |torrent| {
        torrent.category = "Other".to_owned();
    });

    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "category-rule",
            "name": "Move source category",
            "enabled": true,
            "event": "added",
            "action": "set_category",
            "category": "Source",
            "target_category": "Target",
            "tracker": null,
            "command": null,
            "url": null,
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = client
        .post(url(addr, "/api/v1/workflows/category-rule"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = response.json().await.unwrap();
    assert_eq!(result["applied"], serde_json::json!(["category-match"]));
    assert_eq!(
        db.get("category-match").unwrap().unwrap().category,
        "Target"
    );
    assert_eq!(db.get("category-other").unwrap().unwrap().category, "Other");
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn native_legacy_set_category_rule_keeps_its_target() {
    let mutations = Arc::new(AtomicUsize::new(0));
    let (addr, client, db) = spawn_server_with_backend(
        Config::test_default(),
        successful_backend_with_rule_counter(mutations.clone()),
    )
    .await;
    seed_torrent_with(&db, "legacy-category", "Legacy category", |torrent| {
        torrent.category = "Source".to_owned();
    });

    let response = client
        .post(url(addr, "/api/v1/workflows"))
        .json(&serde_json::json!({
            "id": "legacy-category-rule",
            "name": "Legacy target",
            "enabled": true,
            "event": "added",
            "action": "set_category",
            "category": "Target",
            "tracker": null,
            "command": null,
            "url": null,
            "target_path": null
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rules: serde_json::Value = response.json().await.unwrap();
    assert_eq!(rules[0]["category"], serde_json::Value::Null);
    assert_eq!(rules[0]["target_category"], "Target");

    let response = client
        .post(url(addr, "/api/v1/workflows/legacy-category-rule"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = response.json().await.unwrap();
    assert_eq!(result["applied"], serde_json::json!(["legacy-category"]));
    assert_eq!(
        db.get("legacy-category").unwrap().unwrap().category,
        "Target"
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 1);
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
    let (addr, client, db) = spawn_server_with_backend(
        Config::test_default(),
        Arc::new(SuccessfulBackend {
            add_mutations: None,
            tag_mutations: None,
            tracker_mutations: None,
            peer_mutations: None,
            rule_mutations: None,
        }),
    )
    .await;
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

    let form = reqwest::multipart::Form::new()
        .text("paused", "false")
        .text("paused", "true");
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let too_many_urls = std::iter::repeat_n("magnet:?xt=urn:btih:abc", 1_025)
        .collect::<Vec<_>>()
        .join("\n");
    let form = reqwest::multipart::Form::new().text("urls", too_many_urls);
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let too_long_url = format!("magnet:{}", "x".repeat(8_192));
    let form = reqwest::multipart::Form::new().text("urls", too_long_url);
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let oversized_category =
        reqwest::multipart::Form::new().text("category", "x".repeat(1024 * 1024));
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/add"))
        .multipart(oversized_category)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn qb_file_priority_indices_are_bounded_before_backend_calls() {
    let (addr, client, db) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    seed_torrent(&db, "file-prio", "File priority");

    let too_many = std::iter::repeat_n("0", 4_097)
        .collect::<Vec<_>>()
        .join("|");
    let res = client
        .post(url(addr, "/api/qb/v2/torrents/filePrio"))
        .form(&[
            ("hash", "file-prio"),
            ("id", too_many.as_str()),
            ("priority", "1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/filePrio"))
        .form(&[
            ("hash", "file-prio"),
            ("id", &"9".repeat(21)),
            ("priority", "1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// --- Bulk actions (empty hash list → OK with no-op) ---

#[tokio::test]
async fn bulk_dry_run() {
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "abc123", "ABC");
    seed_torrent(&db, "def456", "DEF");
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
async fn bulk_action_bounds_hash_count_and_individual_hash_size() {
    let (addr, client, _) =
        spawn_server_with_backend(Config::test_default(), successful_backend()).await;
    let hashes = (0..=10_000)
        .map(|index| format!("missing-{index}"))
        .collect::<Vec<_>>();
    let too_many = client
        .post(url(addr, "/api/v1/bulk/start"))
        .json(&serde_json::json!({ "hashes": hashes }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_many.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let too_long = client
        .post(url(addr, "/api/v1/bulk/start"))
        .json(&serde_json::json!({ "hashes": ["x".repeat(257)] }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let long_category = client
        .post(url(addr, "/api/v1/bulk/set-category"))
        .json(&serde_json::json!({ "hashes": [], "category": "c".repeat(257) }))
        .send()
        .await
        .unwrap();
    assert_eq!(long_category.status(), StatusCode::BAD_REQUEST);

    let long_path = client
        .post(url(addr, "/api/v1/bulk/set-location"))
        .json(&serde_json::json!({ "hashes": [], "save_path": "p".repeat(4097) }))
        .send()
        .await
        .unwrap();
    assert_eq!(long_path.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bulk_stop_processes_every_hash_exactly_once_concurrently() {
    // bulk_action() uses a bounded worker window instead of allocating one
    // task per hash -- this guards against a concurrency bug losing or
    // duplicating results. The backend points at a nonexistent
    // socket, so every call fails, but every one of the 200 hashes must
    // still show up exactly once, in applied+errors combined.
    let (addr, client, db) = spawn_server_with_db().await;
    let hashes: Vec<String> = (0..200).map(|i| format!("hash{i:04}")).collect();
    for hash in &hashes {
        seed_torrent(&db, hash, hash);
    }
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
    let (addr, client, db) = spawn_server_with_db().await;

    let res = client
        .post(url(addr, "/api/v1/bulk/set-category"))
        .json(&serde_json::json!({ "hashes": ["abc"], "dry_run": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    seed_torrent(&db, "abc", "ABC");
    seed_torrent(&db, "def", "DEF");

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

    for expected_id in 2..=10 {
        let pattern = format!("result-{expected_id}");
        let created: serde_json::Value = client
            .post(url(addr, "/api/qb/v2/search/start"))
            .form(&[("pattern", pattern.as_str())])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(created["id"], expected_id);
    }
    let latest: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/results"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(latest["id"], 10, "latest job is selected numerically");
    assert_eq!(latest["pattern"], "result-10");

    let oversized_pattern = "x".repeat(4_097);
    let res = client
        .post(url(addr, "/api/qb/v2/search/start"))
        .form(&[("pattern", oversized_pattern.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

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
async fn qb_search_plugin_capacity_is_bounded_and_updates_are_atomic() {
    let (addr, client) = spawn_server().await;
    let sources = (0..256)
        .map(|index| format!("https://plugins.example/{index}.py"))
        .collect::<Vec<_>>()
        .join("|");
    let res = client
        .post(url(addr, "/api/qb/v2/search/installPlugin"))
        .form(&[("sources", sources.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/search/installPlugin"))
        .form(&[("sources", "https://plugins.example/overflow.py")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    let res = client
        .post(url(addr, "/api/qb/v2/search/enablePlugin"))
        .form(&[("names", "0.py,overflow.py"), ("enable", "false")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    let plugins: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/plugins"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plugins.as_array().unwrap().len(), 256);
    assert_eq!(
        plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["name"] == "0.py")
            .unwrap()["enabled"],
        true,
        "capacity rejection does not partially apply enable requests"
    );
}

#[tokio::test]
async fn qb_search_job_history_is_bounded_and_evicts_oldest_numeric_id() {
    let (addr, client) = spawn_server().await;
    let mut ids = Vec::new();
    for index in 1..=257 {
        let pattern = format!("query-{index}");
        let job: serde_json::Value = client
            .post(url(addr, "/api/qb/v2/search/start"))
            .form(&[("pattern", pattern.as_str())])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(job["id"].as_u64().unwrap());
    }
    assert_eq!(ids[0], 1);
    assert_eq!(ids[256], 257);

    let evicted = client
        .get(url(addr, "/api/qb/v2/search/results?id=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(evicted.status(), StatusCode::NOT_FOUND);

    let latest: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/search/results"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(latest["id"], 257);
    assert_eq!(latest["pattern"], "query-257");
}

#[tokio::test]
async fn qb_rss_items_are_durable_bounded_and_reject_oversized_keys() {
    let (addr, client, db) = spawn_server_with_db().await;
    db.run_blocking("seed_qbit_rss_items", |db| {
        db.update_qbit_rss_items(|items| {
            for index in 0..1_024 {
                let path = format!("folder/{index}");
                items.insert(
                    path.clone(),
                    serde_json::json!({"uid": path, "type": "folder"}),
                );
            }
        })
    })
    .await
    .unwrap();

    let oversized_path = "x".repeat(8_193);
    let res = client
        .post(url(addr, "/api/qb/v2/rss/addFolder"))
        .form(&[("path", oversized_path.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = client
        .post(url(addr, "/api/qb/v2/rss/addFolder"))
        .form(&[("path", "folder/overflow")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

    let items: serde_json::Value = client
        .get(url(addr, "/api/qb/v2/rss/items"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(items.as_object().unwrap().len(), 1_024);
    assert!(items.get("folder/overflow").is_none());
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
