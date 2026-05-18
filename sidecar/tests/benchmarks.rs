//! Ignored synthetic benchmark checks for Track 1 roadmap targets.
//!
//! Run with:
//! `cargo test --test benchmarks -- --ignored --nocapture`

use axum::Router;
use reqwest::Client;
use std::{net::SocketAddr, sync::Arc, time::Instant};
use tokio::{net::TcpListener, sync::broadcast};

use torrentng::{
    api::{server::AppState, ws::Event},
    cache::{Db, TorrentRow},
    config::Config,
    metrics::Metrics,
};

const DEFAULT_TORRENTS: usize = 50_000;

async fn spawn_server_with_db() -> (SocketAddr, Client, Arc<Db>) {
    let cfg = Arc::new(Config::test_default());
    let db_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let db = Arc::new(Db::open(db_path.as_ref()).unwrap());
    let (tx, _) = broadcast::channel::<Event>(16);
    let metrics = Metrics::new();
    let rt = Arc::new(torrentng::rtorrent::Client::new_unix("/nonexistent", 1));
    let backend = Arc::new(torrentng::backend::rtorrent::RtorrentBackend::new(
        rt.clone(),
    ));
    let state = AppState {
        cfg,
        rt,
        backend,
        db: db.clone(),
        events: tx,
        metrics,
        qbit_search_plugins: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_search_jobs: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
        qbit_next_search_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        qbit_rss_items: Arc::new(tokio::sync::RwLock::new(serde_json::Map::new())),
    };
    let app: Router = torrentng::api::server::build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = Client::builder().cookie_store(true).build().unwrap();
    (addr, client, db)
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

fn torrent_count() -> usize {
    std::env::var("TNG_BENCH_TORRENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_TORRENTS)
}

fn seed_torrents(db: &Db, count: usize) {
    for i in 0..count {
        db.upsert(&TorrentRow {
            hash: format!("bench-{i:08x}"),
            name: format!("Benchmark Torrent {i:08}"),
            size_bytes: 1_000_000,
            bytes_done: if i % 3 == 0 { 1_000_000 } else { 500_000 },
            down_rate: if i % 7 == 0 { 1024 } else { 0 },
            up_rate: if i % 5 == 0 { 2048 } else { 0 },
            up_total: i as i64 * 100,
            down_total: i as i64 * 50,
            ratio: 1000,
            is_active: i % 4 == 0,
            is_open: i % 2 == 0,
            complete: i % 3 == 0,
            state: 0,
            priority: 0,
            category: if i % 2 == 0 { "Movies" } else { "TV" }.to_owned(),
            base_path: "/data".to_owned(),
            directory: format!("/data/bench/{i:08}"),
            creation_date: i as i64,
            timestamp_finished: 0,
            tracker_focus: 0,
            peers_connected: (i % 20) as i64,
            peers_complete: (i % 10) as i64,
            message: String::new(),
            tracker_url: "udp://tracker.example/announce".to_owned(),
            tags: String::new(),
            updated_at: i as i64 + 1,
        })
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "synthetic performance benchmark; run explicitly"]
async fn bench_qb_torrents_info_50k_under_500ms() {
    let (addr, client, db) = spawn_server_with_db().await;
    let count = torrent_count();
    seed_torrents(&db, count);

    let started = Instant::now();
    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/torrents/info?limit={count}&sort=name"),
        ))
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.status(), 200);
    let body: Vec<serde_json::Value> = res.json().await.unwrap();
    assert_eq!(body.len(), count);
    println!("qBit torrents/info {count} rows: {elapsed:?}");
    assert!(
        elapsed.as_millis() < 500,
        "qBit torrents/info exceeded 500ms target: {elapsed:?}"
    );
}

#[tokio::test]
#[ignore = "synthetic performance benchmark; run explicitly"]
async fn bench_qb_sync_maindata_delta_under_50ms() {
    let (addr, client, db) = spawn_server_with_db().await;
    let count = torrent_count();
    seed_torrents(&db, count);

    let started = Instant::now();
    let res = client
        .get(url(
            addr,
            &format!("/api/qb/v2/sync/maindata?rid={}", count.saturating_sub(100)),
        ))
        .send()
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["full_update"], false);
    println!("qBit sync/maindata delta at {count} rows: {elapsed:?}");
    assert!(
        elapsed.as_millis() < 50,
        "qBit sync/maindata delta exceeded 50ms target: {elapsed:?}"
    );
}
