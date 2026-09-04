use axum::{
    extract::{State, WebSocketUpgrade},
    response::IntoResponse,
};
use serde::Serialize;
use std::time::Duration;
use tokio::time::timeout;

use super::server::AppState;

const WS_SEND_TIMEOUT: Duration = Duration::from_secs(5);

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
        upload_total: i64,
        download_total: i64,
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
                        if !send_text(&mut socket, payload).await {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // A lagged broadcast receiver cannot reconstruct the
                        // missing events. Tell the client to reload its
                        // bounded list projection instead of silently
                        // presenting a permanently stale view.
                        let payload = serde_json::json!({
                            "type": "resync_required",
                            "reason": "event_stream_lagged",
                            "dropped": skipped,
                        })
                        .to_string();
                        if !send_text(&mut socket, payload).await {
                            break;
                        }
                    }
                }
            }
        }
    }
}

async fn send_text(socket: &mut axum::extract::ws::WebSocket, payload: String) -> bool {
    matches!(
        timeout(
            WS_SEND_TIMEOUT,
            socket.send(axum::extract::ws::Message::Text(payload.into()))
        )
        .await,
        Ok(Ok(()))
    )
}
