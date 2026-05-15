//! Integration tests for the qBittorrent compatibility layer.
//!
//! These tests spin up the full axum router against an in-memory SQLite DB.
//! They do NOT require a running rTorrent instance — endpoints that touch rTorrent
//! are skipped unless RTORRENT_SOCKET is set.

use std::{net::SocketAddr, sync::Arc};
use axum::Router;
use reqwest::Client;
use tokio::{net::TcpListener, sync::broadcast};

// Re-use internal modules via the binary crate root.
use rtorrentng::{
    api::{server::AppState, ws::Event},
    cache::Db,
    config::Config,
    metrics::Metrics,
};

async fn spawn_server() -> (SocketAddr, Client) {
    let cfg = Arc::new(Config::test_default());
    let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let db = Arc::new(Db::open(db_path.as_ref()).unwrap());
    let (tx, _) = broadcast::channel::<Event>(16);
    let metrics = Metrics::new();

    // Stub rTorrent client pointing at a non-existent socket.
    // Tests that call rTorrent fail gracefully — we only exercise DB-backed endpoints here.
    let rt = Arc::new(rtorrentng::rtorrent::Client::new_unix("/nonexistent", 1));

    let state = AppState { cfg, rt, db, events: tx, metrics };
    let app: Router = rtorrentng::api::server::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();

    (addr, client)
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

// --- qBit auth ---

#[tokio::test]
async fn qb_login_accepts_any_credentials() {
    let (addr, client) = spawn_server().await;
    let res = client
        .post(url(addr, "/api/qb/v2/auth/login"))
        .form(&[("username", "admin"), ("password", "wrong")])
        .send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.text().await.unwrap(), "Ok.");
}

// --- qBit app ---

#[tokio::test]
async fn qb_version() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/qb/v2/app/version")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(!body.is_empty());
}

#[tokio::test]
async fn qb_api_version() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/qb/v2/app/webapiVersion")).send().await.unwrap();
    assert_eq!(res.status(), 200);
}

// --- Categories / Tags (DB-backed, no rTorrent needed) ---

#[tokio::test]
async fn categories_round_trip() {
    let (addr, client) = spawn_server().await;

    // Start empty
    let res = client.get(url(addr, "/api/qb/v2/torrents/categories")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_object().unwrap().is_empty());

    // Create via native API
    let res = client
        .post(url(addr, "/api/v1/categories"))
        .json(&serde_json::json!({ "name": "Movies", "save_path": "/data/movies" }))
        .send().await.unwrap();
    assert_eq!(res.status(), 200);

    // Appears in qBit categories
    let res = client.get(url(addr, "/api/qb/v2/torrents/categories")).send().await.unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("Movies").is_some());
    assert_eq!(body["Movies"]["savePath"], "/data/movies");

    // Delete via native API
    let res = client
        .delete(url(addr, "/api/v1/categories/Movies"))
        .send().await.unwrap();
    assert_eq!(res.status(), 204);

    // Gone
    let res = client.get(url(addr, "/api/qb/v2/torrents/categories")).send().await.unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.as_object().unwrap().is_empty());
}

#[tokio::test]
async fn tags_round_trip() {
    let (addr, client) = spawn_server().await;

    // Empty initially
    let res = client.get(url(addr, "/api/qb/v2/torrents/tags")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<String> = res.json().await.unwrap();
    assert!(body.is_empty());

    // Create tag
    let res = client
        .post(url(addr, "/api/v1/tags"))
        .json(&serde_json::json!({ "name": "4k" }))
        .send().await.unwrap();
    assert_eq!(res.status(), 201);

    // Appears in qBit tags
    let res = client.get(url(addr, "/api/qb/v2/torrents/tags")).send().await.unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(body.contains(&"4k".to_string()));

    // Delete
    let res = client.delete(url(addr, "/api/v1/tags/4k")).send().await.unwrap();
    assert_eq!(res.status(), 204);

    let res = client.get(url(addr, "/api/qb/v2/torrents/tags")).send().await.unwrap();
    let body: Vec<String> = res.json().await.unwrap();
    assert!(!body.contains(&"4k".to_string()));
}

// --- Torrent list (empty cache) ---

#[tokio::test]
async fn qb_torrents_info_empty() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/qb/v2/torrents/info")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn native_torrents_list_empty() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/v1/torrents")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["total"], 0);
    assert!(body["torrents"].as_array().unwrap().is_empty());
}

// --- Sync maindata (empty) ---

#[tokio::test]
async fn qb_sync_maindata_empty() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/qb/v2/sync/maindata")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], true);
    assert!(body["torrents"].as_object().unwrap().is_empty());
}

// --- Transfer info ---

#[tokio::test]
async fn qb_transfer_info() {
    let (addr, client) = spawn_server().await;
    let res = client.get(url(addr, "/api/qb/v2/transfer/info")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["connection_status"], "connected");
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

// --- Bulk actions (empty hash list → OK with no-op) ---

#[tokio::test]
async fn bulk_dry_run() {
    let (addr, client) = spawn_server().await;
    let res = client
        .post(url(addr, "/api/v1/bulk/stop"))
        .json(&serde_json::json!({ "hashes": ["abc123", "def456"], "dry_run": true }))
        .send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["applied"].as_array().unwrap().len(), 2);
}
