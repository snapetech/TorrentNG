use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
};
use serde::Serialize;

use super::server::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TorrentAdded {
        hash: String,
    },
    TorrentRemoved {
        hash: String,
    },
    TorrentUpdated {
        hash: String,
    },
    CategoriesUpdated,
    TagsUpdated,
    TrackerHealthUpdated,
    StorageUpdated,
    RatioGroupsUpdated,
    WorkflowsUpdated,
    WorkflowRunsUpdated,
    RssRulesUpdated,
    SavedViewsUpdated,
    Stats {
        upload_speed: i64,
        download_speed: i64,
        connections: usize,
        pending_connections: usize,
        listen_port: u16,
        firewall: String,
        dht: String,
        pex: String,
    },
}

pub async fn handler(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, s.events.subscribe()))
}

async fn handle_socket(
    mut socket: axum::extract::ws::WebSocket,
    mut rx: tokio::sync::broadcast::Receiver<Event>,
) {
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(_)) => {} // ignore client messages for now
                    _ => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(e) => {
                        let payload = serde_json::to_string(&e).unwrap_or_default();
                        if socket.send(axum::extract::ws::Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    }
}
