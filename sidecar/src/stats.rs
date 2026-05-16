use std::{sync::Arc, time::Duration};

use tokio::sync::broadcast;
use tracing::warn;

use crate::{
    api::ws::Event,
    rtorrent::{Client, TransferRates},
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LIVE_SPEEDS_MAX_AGE: Duration = Duration::from_secs(8);
const DEFAULT_INCOMING_PORT: u16 = 50000;

pub async fn run(rt: Arc<Client>, tx: broadcast::Sender<Event>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        let rates = match live_speeds_file() {
            Some(path) => match read_live_speeds(&path) {
                Some(rates) => rates,
                None => probe_transfer_rates(&rt).await,
            },
            None => match tokio::time::timeout(PROBE_TIMEOUT, rt.transfer_rates()).await {
                Ok(Ok(rates)) => rates,
                Ok(Err(e)) => {
                    warn!("transfer stats error: {e:?}");
                    TransferRates::default()
                }
                Err(_) => {
                    warn!("transfer stats probe timed out");
                    TransferRates::default()
                }
            },
        };
        let live = live_status();
        let _ = tx.send(Event::Stats {
            upload_speed: rates.upload,
            download_speed: rates.download,
            connections: live.connections,
            pending_connections: live.pending_connections,
            listen_port: live.listen_port,
            firewall: live.firewall,
            dht: live.dht,
            pex: live.pex,
        });
    }
}

pub fn current_rates(rt: Arc<Client>) -> impl std::future::Future<Output = TransferRates> {
    async move {
        if let Some(path) = live_speeds_file() {
            if let Some(rates) = read_live_speeds(&path) {
                return rates;
            }
        }
        probe_transfer_rates(&rt).await
    }
}

async fn probe_transfer_rates(rt: &Client) -> TransferRates {
    match tokio::time::timeout(PROBE_TIMEOUT, rt.transfer_rates()).await {
        Ok(Ok(rates)) => rates,
        Ok(Err(e)) => {
            warn!("transfer stats error: {e:?}");
            TransferRates::default()
        }
        Err(_) => {
            warn!("transfer stats probe timed out");
            TransferRates::default()
        }
    }
}

fn live_speeds_file() -> Option<String> {
    std::env::var("RTNG_LIVE_SPEEDS_FILE")
        .ok()
        .filter(|path| !path.trim().is_empty())
}

fn read_live_speeds(path: &str) -> Option<TransferRates> {
    #[derive(serde::Deserialize)]
    struct LiveSpeeds {
        download: i64,
        upload: i64,
        updated_at: Option<i64>,
    }

    let raw = std::fs::read_to_string(path).ok()?;
    let speeds: LiveSpeeds = serde_json::from_str(&raw).ok()?;
    if let Some(updated_at) = speeds.updated_at {
        let age = chrono::Utc::now().timestamp().saturating_sub(updated_at);
        if age > LIVE_SPEEDS_MAX_AGE.as_secs() as i64 {
            return None;
        }
    }
    Some(TransferRates {
        download: speeds.download.max(0),
        upload: speeds.upload.max(0),
    })
}

#[derive(Debug, Default)]
struct LiveStatus {
    connections: usize,
    pending_connections: usize,
    listen_port: u16,
    firewall: String,
    dht: String,
    pex: String,
}

fn live_status() -> LiveStatus {
    let listen_port = incoming_port();
    let sockets = tcp_socket_counts(listen_port);
    let (dht, pex) = rtorrent_feature_status();
    let firewall = if sockets.listening && sockets.established > 0 {
        "open"
    } else if sockets.listening {
        "listening"
    } else {
        "closed"
    };

    LiveStatus {
        connections: sockets.established,
        pending_connections: sockets.pending,
        listen_port,
        firewall: firewall.to_owned(),
        dht,
        pex,
    }
}

fn incoming_port() -> u16 {
    std::env::var("RTORRENT_INCOMING_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_INCOMING_PORT)
}

#[derive(Debug, Default)]
struct TcpSocketCounts {
    listening: bool,
    established: usize,
    pending: usize,
}

fn tcp_socket_counts(port: u16) -> TcpSocketCounts {
    let mut counts = TcpSocketCounts::default();
    read_tcp_table("/proc/net/tcp", port, &mut counts);
    read_tcp_table("/proc/net/tcp6", port, &mut counts);
    counts
}

fn read_tcp_table(path: &str, port: u16, counts: &mut TcpSocketCounts) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };

    for line in raw.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let local_port = endpoint_port(fields[1]);
        let remote_port = endpoint_port(fields[2]);
        if local_port != Some(port) && remote_port != Some(port) {
            continue;
        }

        match fields[3] {
            "0A" if local_port == Some(port) => counts.listening = true,
            "01" => counts.established += 1,
            "02" | "03" => counts.pending += 1,
            _ => {}
        }
    }
}

fn endpoint_port(endpoint: &str) -> Option<u16> {
    let (_, port) = endpoint.rsplit_once(':')?;
    u16::from_str_radix(port, 16).ok()
}

fn rtorrent_feature_status() -> (String, String) {
    let mut dht = "unknown".to_owned();
    let mut pex = "unknown".to_owned();

    for path in [
        "/etc/rtorrent/profile.rc",
        "/etc/rtorrent/user.rc",
        "/config/rtorrent.rc",
    ] {
        if let Ok(raw) = std::fs::read_to_string(path) {
            for line in raw.lines() {
                let normalized = line.to_ascii_lowercase().replace(char::is_whitespace, "");
                if let Some(value) = config_switch(
                    &normalized,
                    "dht.mode.set",
                    &["disable", "off", "no"],
                    &["on", "auto", "enable", "yes"],
                ) {
                    dht = value;
                }
                if let Some(value) = config_switch(
                    &normalized,
                    "protocol.pex.set",
                    &["no", "false", "0", "off", "disable"],
                    &["yes", "true", "1", "on", "enable"],
                ) {
                    pex = value;
                }
            }
        }
    }

    (dht, pex)
}

fn config_switch(raw: &str, key: &str, off_values: &[&str], on_values: &[&str]) -> Option<String> {
    for value in off_values {
        if raw.contains(&format!("{key}={value}")) {
            return Some("off".to_owned());
        }
    }
    for value in on_values {
        if raw.contains(&format!("{key}={value}")) {
            return Some("on".to_owned());
        }
    }
    None
}
