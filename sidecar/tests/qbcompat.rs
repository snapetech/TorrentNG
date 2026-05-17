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
use rtorrentng::{
    api::{server::AppState, ws::Event},
    cache::{Db, TorrentRow},
    config::Config,
    metrics::Metrics,
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
    let cfg = Arc::new(cfg);
    let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let db = Arc::new(Db::open(db_path.as_ref()).unwrap());
    let (tx, _) = broadcast::channel::<Event>(16);
    let metrics = Metrics::new();

    // Stub rTorrent client pointing at a non-existent socket.
    // Tests that call rTorrent fail gracefully — we only exercise DB-backed endpoints here.
    let rt = Arc::new(rtorrentng::rtorrent::Client::new_unix("/nonexistent", 1));

    let state = AppState {
        cfg,
        rt,
        db: db.clone(),
        events: tx.clone(),
        metrics,
    };
    let app: Router = rtorrentng::api::server::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::builder().cookie_store(true).build().unwrap();

    (addr, client, db, tx)
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
    let (addr, client, db) = spawn_server_with_db().await;
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
    let (addr, client, db) = spawn_server_with_db().await;
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
        "/api/qb/v2/app/preferences",
        "/api/qb/v2/transfer/info",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
    }

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
    let (addr, client, db) = spawn_server_with_db().await;
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
    let (addr, client, db) = spawn_server_with_db().await;
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
            .form(&[("hashes", "cross-seed")])
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
    let (addr, client, db, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
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
    let (addr, client, db, tx) = spawn_server_with_config_and_events(Config::test_default()).await;
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
    let (addr, client, db) = spawn_server_with_db().await;
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

    let res = client
        .get(url(addr, "/api/qb/v2/torrents/properties"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
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
    let (addr, client) = spawn_server().await;

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
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["connection_status"], "connected");
    assert_eq!(body["dl_info_speed"], 0);
    assert_eq!(body["up_info_speed"], 0);
    assert_eq!(body["dl_info_data"], 0);
    assert_eq!(body["up_info_data"], 0);
    assert_eq!(body["dl_rate_limit"], 0);
    assert_eq!(body["up_rate_limit"], 0);
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
        assert!(roots[0]["error"]
            .as_str()
            .unwrap()
            .contains("unsupported"));
    }
    assert_eq!(roots[1]["ok"], false);
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
    let (addr, client, db) = spawn_server_with_db().await;
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
    let (addr, client, db) = spawn_server_with_db().await;
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
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Fails.");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/addTrackers"))
        .form(&[("hashes", ""), ("urls", "udp://tracker.example/announce")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

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
    assert_eq!(res.status(), 200);

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
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/toggleSequentialDownload"))
        .form(&[("hashes", "")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn qb_hashes_all_expands_from_cache() {
    let (addr, client, db) = spawn_server_with_db().await;
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
    let (addr, client, db) = spawn_server_with_db().await;
    seed_torrent(&db, "meta-hash", "Metadata");
    seed_torrent(&db, "tag-delta-hash", "Tag Delta");

    let res = client
        .post(url(addr, "/api/qb/v2/torrents/setCategory"))
        .form(&[("hashes", "meta-hash"), ("category", "Movies")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=1"))
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
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=1"))
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
        .get(url(addr, "/api/qb/v2/sync/maindata?rid=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["torrents"]["tag-delta-hash"]["tags"], "");
}

#[tokio::test]
async fn qb_set_tags_replaces_cache_tags() {
    let (addr, client, db) = spawn_server_with_db().await;
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
    let (addr, client) = spawn_server().await;

    for path in [
        "/api/qb/v2/torrents/webseeds?hash=abc",
        "/api/qb/v2/torrents/pieceStates?hash=abc",
        "/api/qb/v2/torrents/pieceHashes?hash=abc",
        "/api/qb/v2/log/main",
        "/api/qb/v2/log/peers",
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
    assert_eq!(body["id"], 0);

    for path in [
        "/api/qb/v2/transfer/speedLimitsMode",
        "/api/qb/v2/transfer/downloadLimit",
        "/api/qb/v2/transfer/uploadLimit",
    ] {
        let res = client.get(url(addr, path)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(res.text().await.unwrap(), "0");
    }
}

#[tokio::test]
async fn qb_set_preferences_validates_json() {
    let (addr, client) = spawn_server().await;

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[("json", r#"{"queueing_enabled":false}"#)])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let res = client
        .post(url(addr, "/api/qb/v2/app/setPreferences"))
        .form(&[("json", "{bad json")])
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}
