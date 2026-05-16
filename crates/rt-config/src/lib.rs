use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Top-level daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub network: NetworkConfig,
    pub storage: StorageConfig,
    pub tracker: TrackerConfig,
    pub dht: DhtConfig,
    pub db: DbConfig,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Directory where .torrent files and session state are stored.
    pub session_dir: PathBuf,
    /// Bind address for the REST/qBit API.
    pub api_bind: String,
    /// Log level filter (e.g. "info", "debug", "rt_engine=trace").
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// TCP port to listen for incoming peer connections.
    pub listen_port: u16,
    /// Maximum total peer connections across all torrents.
    pub max_peers: usize,
    /// Maximum upload rate in bytes/sec (0 = unlimited).
    pub upload_rate_limit: u64,
    /// Maximum download rate in bytes/sec (0 = unlimited).
    pub download_rate_limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Default directory for downloaded content.
    pub download_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// HTTP announce timeout in seconds.
    pub http_timeout_secs: u64,
    /// UDP announce timeout in seconds.
    pub udp_timeout_secs: u64,
    /// Minimum announce interval override in seconds (0 = use tracker's value).
    pub min_interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DhtConfig {
    pub enabled: bool,
    /// UDP port for DHT. 0 = same as listen_port.
    pub port: u16,
    /// Bootstrap nodes as "host:port" strings.
    pub bootstrap_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DbConfig {
    /// Path to the SQLite database file. Empty = session_dir/state.db.
    pub path: PathBuf,
    /// SQLite WAL checkpoint threshold (pages).
    pub wal_checkpoint_pages: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthConfig {
    /// Pre-shared bearer/session tokens accepted by the native API.
    pub api_tokens: Vec<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            session_dir: default_session_dir(),
            api_bind: "127.0.0.1:8080".to_owned(),
            log_level: "info".to_owned(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            listen_port: 6881,
            max_peers: 200,
            upload_rate_limit: 0,
            download_rate_limit: 0,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            download_dir: dirs_default_download(),
        }
    }
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig {
            http_timeout_secs: 30,
            udp_timeout_secs: 15,
            min_interval_secs: 0,
        }
    }
}

impl Default for DhtConfig {
    fn default() -> Self {
        DhtConfig {
            enabled: true,
            port: 0,
            bootstrap_nodes: vec![
                "dht.transmissionbt.com:6881".to_owned(),
                "router.bittorrent.com:6881".to_owned(),
                "router.utorrent.com:6881".to_owned(),
            ],
        }
    }
}

impl Default for DbConfig {
    fn default() -> Self {
        DbConfig {
            path: PathBuf::new(), // resolved relative to session_dir at runtime
            wal_checkpoint_pages: 1000,
        }
    }
}

impl Config {
    /// Load from a TOML file, falling back to defaults for missing fields.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Load from the standard search path, returning defaults if no file exists.
    pub fn load_default() -> Self {
        for path in default_config_paths() {
            if path.exists() {
                match Self::load(&path) {
                    Ok(c) => return c,
                    Err(e) => eprintln!("config error in {}: {e}", path.display()),
                }
            }
        }
        Self::default()
    }

    /// Resolved DB path (falls back to session_dir/state.db).
    pub fn db_path(&self) -> PathBuf {
        if self.db.path == PathBuf::new() {
            self.daemon.session_dir.join("state.db")
        } else {
            self.db.path.clone()
        }
    }

    /// DHT port (falls back to listen_port).
    pub fn dht_port(&self) -> u16 {
        if self.dht.port == 0 {
            self.network.listen_port
        } else {
            self.dht.port
        }
    }
}

fn default_session_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/share/rusttorrentd")
    } else {
        PathBuf::from("/var/lib/rusttorrentd")
    }
}

fn dirs_default_download() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join("Downloads")
    } else {
        PathBuf::from("/tmp")
    }
}

fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(cfg) = std::env::var_os("RUSTTORRENTD_CONFIG") {
        paths.push(PathBuf::from(cfg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/rusttorrentd/config.toml"));
    }
    paths.push(PathBuf::from("/etc/rusttorrentd/config.toml"));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = Config::default();
        assert_eq!(c.network.listen_port, 6881);
        assert_eq!(c.network.max_peers, 200);
        assert!(c.dht.enabled);
        assert!(!c.dht.bootstrap_nodes.is_empty());
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.auth.api_tokens.is_empty());
    }

    #[test]
    fn db_path_fallback() {
        let c = Config::default();
        let p = c.db_path();
        assert!(p.ends_with("state.db"));
    }

    #[test]
    fn dht_port_fallback() {
        let mut c = Config::default();
        c.network.listen_port = 51413;
        assert_eq!(c.dht_port(), 51413);
        c.dht.port = 6882;
        assert_eq!(c.dht_port(), 6882);
    }

    #[test]
    fn parse_toml_partial() {
        let toml = r#"
[network]
listen_port = 51413
max_peers = 500

[auth]
api_tokens = ["one", "two"]
"#;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.network.listen_port, 51413);
        assert_eq!(c.network.max_peers, 500);
        // defaults preserved for unset fields
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.dht.enabled);
        assert_eq!(c.auth.api_tokens, vec!["one", "two"]);
    }
}
