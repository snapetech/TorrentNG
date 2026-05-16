/// Scale certification tests.
///
/// These tests verify that the sidecar API meets throughput targets from
/// docs/ENGINE.md at synthetic load levels.
///
/// Targets (from CLAUDE.md benchmarks section):
///   - 1k torrents:  GET /api/v1/torrents < 100ms
///   - 10k torrents: GET /api/v1/torrents < 200ms
///   - 15k torrents: GET /api/v1/torrents < 500ms
///   - 50k torrents: GET /api/qb/v2/torrents/info < 500ms
///   - native filter/sort over 15k torrents < 250ms
///   - sync/maindata delta < 50ms (at normal churn)
use std::time::Instant;

use axum::{body::Body, http::Request};
use rt_api_native::{build_router, AppState};
use rt_api_qbit::AppState as QbState;
use rt_session::TorrentEntry;
use tower::ServiceExt;

/// Build a native API app populated with `n` synthetic torrents.
async fn native_app_with(n: usize) -> axum::Router {
    let state = AppState::new();
    {
        let mut reg = state.registry.write().await;
        for i in 0..n {
            let hash = format!("{:040x}", i);
            let entry = TorrentEntry::new(hash, format!("torrent_{i}"), "/data".into());
            reg.add(entry).unwrap();
        }
    }
    build_router(state)
}

/// Build a qBit API app populated with `n` synthetic torrents.
async fn qbit_app_with(n: usize) -> axum::Router {
    let state = QbState::new();
    {
        let mut reg = state.registry.write().await;
        for i in 0..n {
            let hash = format!("{:040x}", i);
            let entry = TorrentEntry::new(hash, format!("torrent_{i}"), "/data".into());
            reg.add(entry).unwrap();
        }
    }
    rt_api_qbit::build_qbit_router(state)
}

/// Debug builds are ~15x slower; multiply thresholds so CI passes without --release.
fn threshold(release_ms: u128) -> u128 {
    if cfg!(debug_assertions) {
        release_ms * 20
    } else {
        release_ms
    }
}

async fn get_ms(app: axum::Router, uri: &str) -> u128 {
    let t0 = Instant::now();
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    // Drain body so timing includes serialization.
    let _ = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap();
    t0.elapsed().as_millis()
}

#[tokio::test]
async fn list_1k_torrents_under_100ms() {
    let app = native_app_with(1_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(100);
    assert!(ms < limit, "1k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn list_10k_torrents_under_200ms() {
    let app = native_app_with(10_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(200);
    assert!(ms < limit, "10k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn list_15k_torrents_under_500ms() {
    let app = native_app_with(15_000).await;
    let ms = get_ms(app, "/api/v1/torrents").await;
    let limit = threshold(500);
    assert!(ms < limit, "15k list took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn cold_db_load_15k_under_120s() {
    let dataset = rt_testkit::SyntheticTorrentDataset::new(15_000);
    let conn = rt_testkit::memory_db().unwrap();

    let t0 = Instant::now();
    dataset.write_to_db(&conn).unwrap();
    let rows = rt_db::list_all(&conn).unwrap();
    let ms = t0.elapsed().as_millis();

    assert_eq!(rows.len(), 15_000);
    let limit = threshold(120_000);
    assert!(ms < limit, "15k cold DB load took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn native_filter_sort_15k_under_250ms() {
    let app = native_app_with(15_000).await;
    let ms = get_ms(
        app,
        "/api/v1/torrents?filter=all&sort=name&reverse=true&limit=200&offset=2000",
    )
    .await;
    let limit = threshold(250);
    assert!(
        ms < limit,
        "15k native filter/sort took {ms}ms, want <{limit}ms"
    );
}

#[tokio::test]
async fn qbit_info_50k_under_500ms() {
    let app = qbit_app_with(50_000).await;
    let ms = get_ms(app, "/api/qb/v2/torrents/info").await;
    let limit = threshold(500);
    assert!(ms < limit, "50k qbit/info took {ms}ms, want <{limit}ms");
}

#[tokio::test]
async fn sync_maindata_15k_under_50ms() {
    let app = qbit_app_with(15_000).await;
    let ms = get_ms(app, "/api/qb/v2/sync/maindata").await;
    let limit = threshold(50);
    assert!(ms < limit, "sync/maindata 15k took {ms}ms, want <{limit}ms");
}
