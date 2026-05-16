use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, routing::post, Json, Router};
use base64::{engine::general_purpose, Engine as _};
use rt_engine::EngineHandle;
use rt_metainfo::{parse_magnet, parse_torrent};
use rt_session::SessionRegistry;
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

pub fn build_deluge_router(state: AppState) -> Router {
    Router::new()
        .route("/json", post(json_rpc))
        .route("/deluge/json", post(json_rpc))
        .with_state(state)
}

pub async fn json_rpc(
    State(state): State<AppState>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let result = dispatch(&state, &req.method, &req.params).await;
    let payload = match result {
        Ok(result) => json!({
            "id": req.id,
            "result": result,
            "error": null,
        }),
        Err(message) => json!({
            "id": req.id,
            "result": null,
            "error": {
                "message": message,
                "code": 1,
            },
        }),
    };
    Json(payload)
}

async fn dispatch(state: &AppState, method: &str, params: &[Value]) -> Result<Value, String> {
    match method {
        "auth.login" => Ok(json!(true)),
        "auth.check_session" => Ok(json!(true)),
        "web.connected" => Ok(json!(true)),
        "web.get_host_status" => Ok(json!(["rtorrentNG", "127.0.0.1", 0, "Online"])),
        "web.get_hosts" => Ok(json!([["rtorrentNG", "127.0.0.1", 0, "rtorrentNG"]])),
        "web.connect" | "web.disconnect" | "web.start_daemon" | "web.stop_daemon" => {
            Ok(json!(true))
        }
        "web.get_events" => Ok(json!([])),
        "web.update_ui" => update_ui(state).await,
        "core.get_session_status" => session_status(state).await,
        "core.get_stats" => session_status(state).await,
        "core.get_num_connections" => Ok(json!(0)),
        "core.get_download_rate" => Ok(json!(0.0)),
        "core.get_upload_rate" => Ok(json!(0.0)),
        "core.get_filter_tree" => filter_tree(state).await,
        "core.get_torrents_status" => torrents_status(state).await,
        "core.get_torrent_status" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            torrent_status(state, hash).await
        }
        "core.pause_torrent" => {
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.pause_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.resume_torrent" => {
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.resume_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.force_recheck" => {
            for hash in string_list(params.first()) {
                if let Some(engine) = &state.engine {
                    let _ = engine.recheck_torrent(hash).await;
                }
            }
            Ok(json!(true))
        }
        "core.remove_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let remove_data = params.get(1).and_then(Value::as_bool).unwrap_or(false);
            if let Some(engine) = &state.engine {
                let _ = engine.remove_torrent(hash.to_owned(), remove_data).await;
            }
            Ok(json!(true))
        }
        "core.add_torrent_magnet" => {
            let uri = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing magnet URI".to_owned())?;
            add_magnet(state, uri, params.get(1)).await
        }
        "core.add_torrent_file" => {
            let data = params
                .get(1)
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent data".to_owned())?;
            add_torrent_file(state, data, params.get(2)).await
        }
        "core.set_torrent_options" => Ok(json!(true)),
        "label.get_labels" => labels(state).await,
        "label.add" => Ok(json!(true)),
        "label.remove" => Ok(json!(true)),
        "label.set_options" => Ok(json!(true)),
        "label.set_torrent" => {
            let hash = params
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "missing torrent id".to_owned())?;
            let label = params.get(1).and_then(Value::as_str).unwrap_or_default();
            set_label(state, hash, label).await?;
            Ok(json!(true))
        }
        "core.get_free_space" => Ok(json!(0)),
        "core.get_config" => Ok(json!({
            "download_location": "/downloads",
            "move_completed": false,
            "max_download_speed": -1.0,
            "max_upload_speed": -1.0,
        })),
        "core.get_enabled_plugins" => Ok(json!([])),
        _ => Err(format!("unsupported method {method}")),
    }
}

async fn session_status(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let torrent_count = reg.iter().count();
    let total_payload_download = reg.iter().fold(0_u64, |acc, entry| {
        acc.saturating_add(entry.stats.downloaded)
    });
    let total_payload_upload = reg
        .iter()
        .fold(0_u64, |acc, entry| acc.saturating_add(entry.stats.uploaded));
    Ok(json!({
        "payload_download_rate": 0.0,
        "payload_upload_rate": 0.0,
        "download_rate": 0.0,
        "upload_rate": 0.0,
        "num_connections": 0,
        "total_payload_download": total_payload_download,
        "total_payload_upload": total_payload_upload,
        "num_torrents": torrent_count,
    }))
}

async fn filter_tree(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    let mut states = std::collections::BTreeMap::<String, usize>::new();
    for entry in reg.iter() {
        *labels
            .entry(entry.category.clone().unwrap_or_default())
            .or_default() += 1;
        *states
            .entry(deluge_state(entry.state.as_str()).to_owned())
            .or_default() += 1;
    }
    Ok(json!({
        "label": labels.into_iter().map(|(label, count)| json!([label, count])).collect::<Vec<_>>(),
        "state": states.into_iter().map(|(state, count)| json!([state, count])).collect::<Vec<_>>(),
    }))
}

async fn update_ui(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let torrents = reg
        .iter()
        .map(|entry| (entry.info_hash.clone(), deluge_torrent(entry)))
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({
        "connected": true,
        "torrents": torrents,
        "filters": {
            "state": [["All", reg.iter().count()]],
            "label": labels_from_registry(&reg),
        },
        "stats": {
            "download_rate": 0.0,
            "upload_rate": 0.0,
            "num_connections": 0,
        }
    }))
}

async fn labels(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    Ok(json!(reg
        .iter()
        .filter_map(|entry| entry.category.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()))
}

async fn set_label(state: &AppState, hash: &str, label: &str) -> Result<(), String> {
    let label = label.trim();
    let category = if label.is_empty() {
        None
    } else {
        Some(label.to_owned())
    };
    if let Some(engine) = &state.engine {
        engine
            .update_torrent_labels(hash.to_owned(), Some(category), Vec::new(), Vec::new())
            .await?;
        return Ok(());
    }
    let mut reg = state.registry.write().await;
    let entry = reg
        .get_mut(hash)
        .ok_or_else(|| format!("torrent {hash} not found"))?;
    entry.category = category;
    Ok(())
}

async fn torrents_status(state: &AppState) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let torrents = reg
        .iter()
        .map(|entry| (entry.info_hash.clone(), deluge_torrent(entry)))
        .collect::<serde_json::Map<_, _>>();
    Ok(Value::Object(torrents))
}

async fn torrent_status(state: &AppState, hash: &str) -> Result<Value, String> {
    let reg = state.registry.read().await;
    let entry = reg
        .get(hash)
        .ok_or_else(|| format!("torrent {hash} not found"))?;
    Ok(deluge_torrent(entry))
}

fn deluge_torrent(entry: &rt_session::TorrentEntry) -> Value {
    let progress = if entry.total_length == 0 {
        0.0
    } else {
        entry.total_length.saturating_sub(entry.amount_left) as f64 * 100.0
            / entry.total_length as f64
    };
    json!({
        "hash": entry.info_hash,
        "name": entry.name,
        "state": deluge_state(entry.state.as_str()),
        "progress": progress,
        "total_size": entry.total_length,
        "total_done": entry.total_length.saturating_sub(entry.amount_left),
        "download_payload_rate": 0,
        "upload_payload_rate": 0,
        "ratio": entry.stats.ratio(),
        "save_path": entry.save_path,
        "label": entry.category.clone().unwrap_or_default(),
        "tags": entry.tags,
        "is_finished": entry.completed_at.is_some(),
    })
}

fn labels_from_registry(reg: &SessionRegistry) -> Vec<Value> {
    let mut labels = std::collections::BTreeMap::<String, usize>::new();
    for entry in reg.iter() {
        if let Some(label) = &entry.category {
            *labels.entry(label.clone()).or_default() += 1;
        }
    }
    labels
        .into_iter()
        .map(|(label, count)| json!([label, count]))
        .collect()
}

async fn add_magnet(state: &AppState, uri: &str, options: Option<&Value>) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Err("engine unavailable".to_owned());
    };
    let magnet = parse_magnet(uri).map_err(|e| e.to_string())?;
    let save_path = options
        .and_then(|value| value.get("download_location"))
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let hash = engine
        .add_magnet_with_labels(magnet, save_path, false, None, Vec::new())
        .await?;
    Ok(json!(hash))
}

async fn add_torrent_file(
    state: &AppState,
    data: &str,
    options: Option<&Value>,
) -> Result<Value, String> {
    let Some(engine) = &state.engine else {
        return Err("engine unavailable".to_owned());
    };
    let raw = general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())?;
    let meta = parse_torrent(&raw).map_err(|e| e.to_string())?;
    let save_path = options
        .and_then(|value| value.get("download_location"))
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from);
    let hash = engine
        .add_torrent_with_labels(meta, save_path, false, None, Vec::new())
        .await?;
    Ok(json!(hash))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn deluge_state(state: &str) -> &'static str {
    match state {
        "seeding" => "Seeding",
        "downloading" | "metadata_pending" => "Downloading",
        "checking" => "Checking",
        "paused" | "stopped" => "Paused",
        "error" => "Error",
        _ => "Queued",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use rt_session::TorrentEntry;
    use tower::ServiceExt;

    #[tokio::test]
    async fn deluge_update_ui_projects_registry() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            let mut entry = TorrentEntry::new("a".repeat(40), "alpha".into(), "/data".into());
            entry.total_length = 100;
            entry.amount_left = 25;
            reg.add(entry).unwrap();
        }
        let app = build_deluge_router(AppState::new(registry));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":1,"method":"web.update_ui","params":[[],{}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["error"].is_null());
        assert_eq!(
            body["result"]["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["name"],
            "alpha"
        );
        assert_eq!(
            body["result"]["torrents"]["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["progress"],
            75.0
        );
    }

    #[tokio::test]
    async fn deluge_auth_and_config_are_supported() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        let app = build_deluge_router(AppState::new(registry));
        for method in ["auth.login", "web.connected", "core.get_config"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/deluge/json")
                        .header("content-type", "application/json")
                        .body(Body::from(format!(
                            r#"{{"id":1,"method":"{method}","params":[]}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(resp.status().is_success());
        }
    }

    #[tokio::test]
    async fn deluge_label_methods_update_registry() {
        let registry = Arc::new(RwLock::new(SessionRegistry::new()));
        {
            let mut reg = registry.write().await;
            reg.add(TorrentEntry::new(
                "b".repeat(40),
                "beta".into(),
                "/data".into(),
            ))
            .unwrap();
        }
        let app = build_deluge_router(AppState::new(Arc::clone(&registry)));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"id":1,"method":"label.set_torrent","params":["{}","movies"]}}"#,
                        "b".repeat(40)
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert_eq!(
            registry
                .read()
                .await
                .get(&"b".repeat(40))
                .unwrap()
                .category
                .as_deref(),
            Some("movies")
        );

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/json")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"id":2,"method":"label.get_labels","params":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"], json!(["movies"]));
    }
}
