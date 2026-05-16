use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
use rt_engine::EngineHandle;
use rt_session::SessionRegistry;
use serde_json::{json, Value};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<RwLock<SessionRegistry>>,
    pub engine: Option<EngineHandle>,
}

impl AppState {
    pub fn new(registry: Arc<RwLock<SessionRegistry>>) -> Self {
        Self {
            registry,
            engine: None,
        }
    }

    pub fn with_engine(registry: Arc<RwLock<SessionRegistry>>, engine: EngineHandle) -> Self {
        Self {
            registry,
            engine: Some(engine),
        }
    }
}

pub fn build_transmission_router(state: AppState) -> Router {
    Router::new()
        .route("/transmission/rpc", post(rpc))
        .route("/api/transmission/rpc", post(rpc))
        .with_state(state)
}

async fn rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tag = body.get("tag").cloned();
    let arguments = match method {
        "session-get" => session_get(),
        "torrent-get" => torrent_get(&state).await,
        "torrent-start" | "torrent-stop" | "torrent-verify" | "torrent-reannounce" => json!({}),
        _ => return response(tag, "method name not recognized", json!({})),
    };

    let mut payload = response(tag, "success", arguments).0;
    if let Some(obj) = payload.as_object_mut() {
        if let Some(session) = headers
            .get("x-transmission-session-id")
            .and_then(|h| h.to_str().ok())
        {
            obj.insert("session-id".to_owned(), Value::String(session.to_owned()));
        }
    }
    Json(payload)
}

fn session_get() -> Value {
    json!({
        "version": "rtorrentNG",
        "rpc-version": 17,
        "rpc-version-minimum": 1,
        "download-dir": "/downloads",
        "config-dir": "/config",
        "start-added-torrents": true,
        "trash-original-torrent-files": false,
        "speed-limit-down-enabled": false,
        "speed-limit-up-enabled": false,
    })
}

async fn torrent_get(state: &AppState) -> Value {
    let reg = state.registry.read().await;
    let torrents = reg
        .iter()
        .map(|entry| {
            let completed = entry.total_length.saturating_sub(entry.amount_left);
            json!({
                "hashString": entry.info_hash,
                "name": entry.name,
                "totalSize": entry.total_length,
                "downloadedEver": entry.stats.downloaded,
                "uploadedEver": entry.stats.uploaded,
                "percentDone": if entry.total_length == 0 {
                    0.0
                } else {
                    completed as f64 / entry.total_length as f64
                },
                "rateDownload": 0,
                "rateUpload": 0,
                "status": transmission_status(entry.state),
                "downloadDir": entry.save_path,
                "isFinished": entry.total_length > 0 && entry.amount_left == 0,
            })
        })
        .collect::<Vec<_>>();
    json!({ "torrents": torrents })
}

fn transmission_status(state: rt_session::TorrentState) -> i64 {
    match state {
        rt_session::TorrentState::Stopped | rt_session::TorrentState::Paused => 0,
        rt_session::TorrentState::MetadataPending | rt_session::TorrentState::Queued => 1,
        rt_session::TorrentState::Checking => 2,
        rt_session::TorrentState::Downloading => 4,
        rt_session::TorrentState::Seeding => 6,
        rt_session::TorrentState::Error => 0,
    }
}

fn response(tag: Option<Value>, result: &str, arguments: Value) -> Json<Value> {
    let mut payload = json!({
        "result": result,
        "arguments": arguments,
    });
    if let Some(tag) = tag {
        payload["tag"] = tag;
    }
    Json(payload)
}
