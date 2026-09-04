use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use tokio::sync::broadcast;
use tracing::warn;

use crate::{
    api::ws::Event,
    backend::TorrentBackend,
    cache::{AppEventRow, Db},
    rtorrent::TransferRates,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LIVE_SPEEDS_MAX_AGE: Duration = Duration::from_secs(60);
const MAX_LIVE_SPEEDS_BYTES: u64 = 64 * 1024;
const MAX_RTORRENT_CONFIG_PROBE_BYTES: u64 = 256 * 1024;
const DEFAULT_INCOMING_PORT: u16 = 50000;
static SESSION_UPLOAD_TOTAL: AtomicI64 = AtomicI64::new(0);
static SESSION_DOWNLOAD_TOTAL: AtomicI64 = AtomicI64::new(0);

#[derive(Debug, Clone, Copy, Default)]
pub struct SessionTotals {
    pub upload: i64,
    pub download: i64,
}

pub async fn run(
    backend: Arc<dyn TorrentBackend>,
    db: Arc<Db>,
    tx: broadcast::Sender<Event>,
    interval: Duration,
    event_retention: usize,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_tick = tokio::time::Instant::now();
    let mut upload_total = 0_i64;
    let mut download_total = 0_i64;
    let mut stats_error_active = false;

    loop {
        ticker.tick().await;
        let now = tokio::time::Instant::now();
        let elapsed = now.duration_since(last_tick).as_secs_f64();
        last_tick = now;
        let file_rates = match live_speeds_file() {
            Some(path) => read_live_speeds_async(path).await,
            None => None,
        };
        let rates = match file_rates {
            Some(rates) => rates,
            None => match probe_transfer_rates_result(backend.as_ref()).await {
                Ok(rates) => {
                    if stats_error_active {
                        append_app_event_async(
                            &db,
                            "info",
                            "rtorrent_stats_recovered",
                            "backend transfer stats recovered",
                            serde_json::json!({
                                "component": backend.backend_type().as_str(),
                                "operation": "transfer_stats",
                                "result": "ok",
                            }),
                            event_retention,
                        )
                        .await;
                        stats_error_active = false;
                    }
                    rates
                }
                Err(e) => {
                    record_stats_probe_failure(
                        backend.as_ref(),
                        &db,
                        &mut stats_error_active,
                        &e,
                        event_retention,
                    )
                    .await;
                    TransferRates::default()
                }
            },
        };
        upload_total = upload_total.saturating_add((rates.upload.max(0) as f64 * elapsed) as i64);
        download_total =
            download_total.saturating_add((rates.download.max(0) as f64 * elapsed) as i64);
        SESSION_UPLOAD_TOTAL.store(upload_total, Ordering::Relaxed);
        SESSION_DOWNLOAD_TOTAL.store(download_total, Ordering::Relaxed);
        let live = live_status(backend.as_ref()).await;
        let _ = tx.send(Event::Stats {
            upload_speed: rates.upload,
            download_speed: rates.download,
            upload_total,
            download_total,
            connections: live.connections,
            pending_connections: live.pending_connections,
            listen_port: live.listen_port,
            firewall: live.firewall,
            dht: live.dht,
            pex: live.pex,
        });
    }
}

pub fn session_totals() -> SessionTotals {
    SessionTotals {
        upload: SESSION_UPLOAD_TOTAL.load(Ordering::Relaxed),
        download: SESSION_DOWNLOAD_TOTAL.load(Ordering::Relaxed),
    }
}

pub async fn current_rates(backend: Arc<dyn TorrentBackend>) -> TransferRates {
    current_rates_result(backend).await.unwrap_or_else(|e| {
        warn!(
            component = "stats",
            operation = "current_rates",
            result = "error",
            error = %crate::sync::error_chain(&e),
            "current transfer rates unavailable"
        );
        TransferRates::default()
    })
}

/// Read current rates without converting a dead or unresponsive backend into
/// a plausible-looking zero. Compatibility feeds may still use `current_rates`
/// when they must render a cached projection, but authoritative API handlers
/// should use this result-returning path.
pub async fn current_rates_result(
    backend: Arc<dyn TorrentBackend>,
) -> anyhow::Result<TransferRates> {
    if let Some(path) = live_speeds_file() {
        if let Some(rates) = read_live_speeds_async(path).await {
            return Ok(rates);
        }
    }
    probe_transfer_rates_result(backend.as_ref()).await
}

async fn read_live_speeds_async(path: String) -> Option<TransferRates> {
    match tokio::task::spawn_blocking(move || read_live_speeds(&path)).await {
        Ok(rates) => rates,
        Err(error) => {
            warn!(
                component = "stats",
                operation = "read_live_speeds",
                result = "error",
                error = %error,
                "live speed file worker failed"
            );
            None
        }
    }
}

async fn record_stats_probe_failure(
    backend: &dyn TorrentBackend,
    db: &Db,
    stats_error_active: &mut bool,
    error: &anyhow::Error,
    event_retention: usize,
) {
    let chain = crate::sync::error_chain(error);
    warn!(
        component = backend.backend_type().as_str(),
        operation = "transfer_stats",
        result = "error",
        error = %chain,
        "transfer stats probe failed"
    );
    if !*stats_error_active {
        append_app_event_async(
            db,
            "warn",
            "rtorrent_stats_error",
            "backend transfer stats probe failed",
            serde_json::json!({
                "component": backend.backend_type().as_str(),
                "operation": "transfer_stats",
                "result": "error",
                "error": chain,
            }),
            event_retention,
        )
        .await;
        *stats_error_active = true;
    }
}

async fn probe_transfer_rates_result(
    backend: &dyn TorrentBackend,
) -> anyhow::Result<TransferRates> {
    if let Ok(Ok(summary)) =
        tokio::time::timeout(PROBE_TIMEOUT, backend.live_summary("main", 1)).await
    {
        return Ok(summary.rates);
    }

    match tokio::time::timeout(PROBE_TIMEOUT, backend.transfer_rates()).await {
        Ok(Ok(rates)) => Ok(rates),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::anyhow!(
            "transfer stats probe timed out after {} ms",
            PROBE_TIMEOUT.as_millis()
        )),
    }
}

async fn append_app_event_async(
    db: &Db,
    level: &str,
    kind: &str,
    message: &str,
    payload: serde_json::Value,
    retention: usize,
) {
    let event = AppEventRow {
        event_id: None,
        occurred_at: chrono::Utc::now().timestamp(),
        level: level.to_owned(),
        kind: kind.to_owned(),
        message: message.to_owned(),
        payload: payload.to_string(),
    };
    if let Err(e) = db
        .run_blocking("stats_app_event", move |db| {
            db.append_app_event(&event, retention)
        })
        .await
    {
        warn!(
            component = "app_events",
            operation = "append",
            kind,
            result = "error",
            error = %e,
            "failed to append stats app event"
        );
    }
}

fn live_speeds_file() -> Option<String> {
    std::env::var("TNG_LIVE_SPEEDS_FILE")
        .or_else(|_| std::env::var("RTNG_LIVE_SPEEDS_FILE"))
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

    let raw = read_bounded_text(path, MAX_LIVE_SPEEDS_BYTES).ok()?;
    let speeds: LiveSpeeds = serde_json::from_str(&raw).ok()?;
    let legacy_modified_at = speeds.updated_at.is_none().then(|| {
        std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
    });
    if !is_live_speeds_fresh(speeds.updated_at, legacy_modified_at.flatten()) {
        return None;
    }
    Some(TransferRates {
        download: speeds.download.max(0),
        upload: speeds.upload.max(0),
    })
}

fn read_bounded_text(path: &str, max_bytes: u64) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "live speed file exceeds size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "live speed file is not valid UTF-8",
        )
    })
}

fn is_live_speeds_fresh(updated_at: Option<i64>, legacy_modified_at: Option<SystemTime>) -> bool {
    match updated_at {
        Some(updated_at) => {
            let age = chrono::Utc::now().timestamp().saturating_sub(updated_at);
            age <= LIVE_SPEEDS_MAX_AGE.as_secs() as i64
        }
        None => legacy_modified_at
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age <= LIVE_SPEEDS_MAX_AGE),
    }
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

async fn live_status(backend: &dyn TorrentBackend) -> LiveStatus {
    let listen_port = incoming_port();
    let (dht, pex) = backend.feature_status().await;
    let probe_dht = dht.clone();
    let probe_pex = pex.clone();
    let local_status = tokio::task::spawn_blocking(move || {
        let sockets = tcp_socket_counts(listen_port);
        let firewall = if sockets.listening && sockets.established > 0 {
            "open"
        } else if sockets.listening {
            "listening"
        } else {
            "closed"
        };
        let mut dht = probe_dht;
        let mut pex = probe_pex;
        fill_feature_status_from_config(&mut dht, &mut pex);
        LiveStatus {
            connections: sockets.established,
            pending_connections: sockets.pending,
            listen_port,
            firewall: firewall.to_owned(),
            dht,
            pex,
        }
    })
    .await;
    match local_status {
        Ok(status) => status,
        Err(error) => {
            warn!(
                component = "stats",
                operation = "local_status",
                result = "error",
                error = %error,
                "local stats probe worker failed"
            );
            LiveStatus {
                connections: 0,
                pending_connections: 0,
                listen_port,
                firewall: "unknown".to_owned(),
                dht,
                pex,
            }
        }
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
    let Ok(file) = File::open(path) else {
        return;
    };

    for line in BufReader::new(file).lines().skip(1) {
        let Ok(line) = line else {
            break;
        };
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

fn fill_feature_status_from_config(dht: &mut String, pex: &mut String) {
    for path in [
        "/etc/rtorrent/profile.rc",
        "/etc/rtorrent/user.rc",
        "/config/rtorrent.rc",
    ] {
        if let Ok(raw) = read_bounded_text(path, MAX_RTORRENT_CONFIG_PROBE_BYTES) {
            for line in raw.lines() {
                let normalized = line.to_ascii_lowercase().replace(char::is_whitespace, "");
                if let Some(value) = config_switch(
                    &normalized,
                    "dht.mode.set",
                    &["disable", "off", "no"],
                    &["on", "auto", "enable", "yes"],
                ) {
                    if dht == "unknown" {
                        *dht = value;
                    }
                }
                if let Some(value) = config_switch(
                    &normalized,
                    "protocol.pex.set",
                    &["no", "false", "0", "off", "disable"],
                    &["yes", "true", "1", "on", "enable"],
                ) {
                    if pex == "unknown" {
                        *pex = value;
                    }
                }
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_speeds_with_current_timestamp_are_fresh() {
        assert!(is_live_speeds_fresh(
            Some(chrono::Utc::now().timestamp()),
            None
        ));
    }

    #[test]
    fn live_speeds_with_old_timestamp_are_stale() {
        let old = chrono::Utc::now().timestamp() - LIVE_SPEEDS_MAX_AGE.as_secs() as i64 - 1;
        assert!(!is_live_speeds_fresh(Some(old), None));
    }

    #[test]
    fn legacy_live_speeds_require_fresh_mtime() {
        assert!(is_live_speeds_fresh(None, Some(SystemTime::now())));
        assert!(!is_live_speeds_fresh(None, None));
    }

    #[test]
    fn reads_current_timestamped_live_speeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live-speeds.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "download": 123,
                "upload": 45,
                "updated_at": chrono::Utc::now().timestamp(),
            })
            .to_string(),
        )
        .unwrap();

        let rates = read_live_speeds(path.to_str().unwrap()).unwrap();

        assert_eq!(rates.download, 123);
        assert_eq!(rates.upload, 45);
    }

    #[test]
    fn oversized_live_speeds_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live-speeds.json");
        std::fs::write(&path, vec![b' '; (MAX_LIVE_SPEEDS_BYTES + 1) as usize]).unwrap();
        assert!(read_live_speeds(path.to_str().unwrap()).is_none());
    }
}
