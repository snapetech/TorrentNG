use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("config validation error: {0}")]
    Validation(String),
}

/// Top-level daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub network: NetworkConfig,
    pub storage: StorageConfig,
    pub memory: MemoryConfig,
    pub runtime: RuntimeConfig,
    pub tracker: TrackerConfig,
    pub dht: DhtConfig,
    pub db: DbConfig,
    pub auth: AuthConfig,
    pub logging: rt_logging::LoggingConfig,
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
    /// Max seconds to wait for torrent tasks to send stopped announces on shutdown.
    pub shutdown_timeout_secs: u64,
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
    /// Enable per-device peer-read elevator scheduling where storage profiles benefit.
    pub device_elevator_enabled: bool,
    pub file_pool_size: usize,
    pub idle_file_ttl_secs: u64,
    pub io_worker_threads: usize,
    pub io_queue_depth: usize,
    pub hash_worker_threads: usize,
    pub hash_queue_depth: usize,
    pub preallocation_mode: StoragePreallocationMode,
    pub durability_mode: StorageDurabilityMode,
    pub peer_read_readahead_bytes: usize,
    /// Bounded peer-read readahead cache entries per torrent scheduler.
    pub peer_read_cache_entries: usize,
    pub peer_read_elevator_budget_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoragePreallocationMode {
    Off,
    Auto,
    Sparse,
    Full,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageDurabilityMode {
    Fast,
    Checkpoint,
    Strict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub total_cap_mb: u64,
    pub storage_frame_cap_mb: u64,
    pub queued_disk_cap_mb: u64,
    pub piece_assembly_cap_mb: u64,
    pub peer_buffer_cap_mb: u64,
    pub metadata_cap_mb: u64,
    pub pressure_constrained_pct: u8,
    pub pressure_critical_pct: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub torrent_tiers_enabled: bool,
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
            shutdown_timeout_secs: 10,
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
            device_elevator_enabled: true,
            file_pool_size: 512,
            idle_file_ttl_secs: 300,
            io_worker_threads: 4,
            io_queue_depth: 256,
            hash_worker_threads: 2,
            hash_queue_depth: 256,
            preallocation_mode: StoragePreallocationMode::Auto,
            durability_mode: StorageDurabilityMode::Checkpoint,
            peer_read_readahead_bytes: 512 * 1024,
            peer_read_cache_entries: 64,
            peer_read_elevator_budget_ms: 25,
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        MemoryConfig {
            total_cap_mb: 512,
            storage_frame_cap_mb: 128,
            queued_disk_cap_mb: 64,
            piece_assembly_cap_mb: 128,
            peer_buffer_cap_mb: 128,
            metadata_cap_mb: 32,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            torrent_tiers_enabled: true,
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
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
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

    /// Validate config invariants that would otherwise turn into runtime footguns.
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(!self.daemon.api_bind.trim().is_empty(), "daemon.api_bind must not be empty")?;
        require(self.daemon.shutdown_timeout_secs > 0, "daemon.shutdown_timeout_secs must be greater than zero")?;
        require(self.network.max_peers > 0, "network.max_peers must be greater than zero")?;
        require(!self.storage.download_dir.as_os_str().is_empty(), "storage.download_dir must not be empty")?;
        require(self.storage.file_pool_size > 0, "storage.file_pool_size must be greater than zero")?;
        require(self.storage.io_worker_threads > 0, "storage.io_worker_threads must be greater than zero")?;
        require(self.storage.io_queue_depth > 0, "storage.io_queue_depth must be greater than zero")?;
        require(self.storage.hash_worker_threads > 0, "storage.hash_worker_threads must be greater than zero")?;
        require(self.storage.hash_queue_depth > 0, "storage.hash_queue_depth must be greater than zero")?;
        require(
            self.storage.peer_read_readahead_bytes <= 64 * 1024 * 1024,
            "storage.peer_read_readahead_bytes must be <= 64MiB",
        )?;
        require(
            self.memory.total_cap_mb > 0,
            "memory.total_cap_mb must be greater than zero",
        )?;
        require(
            self.memory.pressure_constrained_pct < self.memory.pressure_critical_pct,
            "memory.pressure_constrained_pct must be less than memory.pressure_critical_pct",
        )?;
        require(
            self.memory.pressure_critical_pct <= 100,
            "memory.pressure_critical_pct must be <= 100",
        )?;
        for (field, value) in [
            ("memory.storage_frame_cap_mb", self.memory.storage_frame_cap_mb),
            ("memory.queued_disk_cap_mb", self.memory.queued_disk_cap_mb),
            ("memory.piece_assembly_cap_mb", self.memory.piece_assembly_cap_mb),
            ("memory.peer_buffer_cap_mb", self.memory.peer_buffer_cap_mb),
            ("memory.metadata_cap_mb", self.memory.metadata_cap_mb),
        ] {
            require(value <= self.memory.total_cap_mb, format!("{field} must be <= memory.total_cap_mb"))?;
        }
        require(
            self.tracker.http_timeout_secs > 0,
            "tracker.http_timeout_secs must be greater than zero",
        )?;
        require(
            self.tracker.udp_timeout_secs > 0,
            "tracker.udp_timeout_secs must be greater than zero",
        )?;
        require(
            self.db.wal_checkpoint_pages > 0,
            "db.wal_checkpoint_pages must be greater than zero",
        )?;
        for token in &self.auth.api_tokens {
            require(!token.trim().is_empty(), "auth.api_tokens must not contain empty tokens")?;
        }
        Ok(())
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

fn require(condition: bool, message: impl Into<String>) -> Result<(), ConfigError> {
    if condition {
        Ok(())
    } else {
        Err(ConfigError::Validation(message.into()))
    }
}

fn default_session_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/share/torrentngd")
    } else {
        PathBuf::from("/var/lib/torrentngd")
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
    if let Some(cfg) = std::env::var_os("TORRENTNGD_CONFIG") {
        paths.push(PathBuf::from(cfg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/torrentngd/config.toml"));
    }
    paths.push(PathBuf::from("/etc/torrentngd/config.toml"));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = Config::default();
        c.validate().unwrap();
        assert_eq!(c.network.listen_port, 6881);
        assert_eq!(c.network.max_peers, 200);
        assert!(c.dht.enabled);
        assert!(!c.dht.bootstrap_nodes.is_empty());
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.auth.api_tokens.is_empty());
        assert_eq!(c.daemon.shutdown_timeout_secs, 10);
        assert_eq!(c.memory.total_cap_mb, 512);
        assert_eq!(c.memory.storage_frame_cap_mb, 128);
        assert_eq!(c.memory.queued_disk_cap_mb, 64);
        assert!(c.runtime.torrent_tiers_enabled);
        assert!(c.storage.device_elevator_enabled);
        assert_eq!(c.storage.file_pool_size, 512);
        assert_eq!(c.storage.idle_file_ttl_secs, 300);
        assert_eq!(c.storage.io_worker_threads, 4);
        assert_eq!(c.storage.io_queue_depth, 256);
        assert_eq!(c.storage.hash_worker_threads, 2);
        assert_eq!(c.storage.hash_queue_depth, 256);
        assert_eq!(c.storage.preallocation_mode, StoragePreallocationMode::Auto);
        assert_eq!(c.storage.durability_mode, StorageDurabilityMode::Checkpoint);
        assert_eq!(c.storage.peer_read_readahead_bytes, 512 * 1024);
        assert_eq!(c.storage.peer_read_cache_entries, 64);
        assert_eq!(c.storage.peer_read_elevator_budget_ms, 25);
        assert_eq!(c.logging, rt_logging::LoggingConfig::default());
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
    fn invalid_config_is_rejected() {
        let mut c = Config::default();
        c.storage.io_worker_threads = 0;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.memory.pressure_constrained_pct = 95;
        c.memory.pressure_critical_pct = 90;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.auth.api_tokens = vec!["".to_owned()];
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));
    }

    #[test]
    fn parse_toml_partial() {
        let toml = r#"
[network]
listen_port = 51413
max_peers = 500

[auth]
api_tokens = ["one", "two"]

[storage]
file_pool_size = 99
idle_file_ttl_secs = 12
io_worker_threads = 3
io_queue_depth = 77
hash_worker_threads = 4
hash_queue_depth = 88
preallocation_mode = "sparse"
durability_mode = "strict"
peer_read_readahead_bytes = 131072
peer_read_cache_entries = 17
peer_read_elevator_budget_ms = 9
"#;
        let c: Config = toml::from_str(toml).unwrap();
        c.validate().unwrap();
        assert_eq!(c.network.listen_port, 51413);
        assert_eq!(c.network.max_peers, 500);
        // defaults preserved for unset fields
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.dht.enabled);
        assert_eq!(c.auth.api_tokens, vec!["one", "two"]);
        assert_eq!(c.storage.file_pool_size, 99);
        assert_eq!(c.storage.idle_file_ttl_secs, 12);
        assert_eq!(c.storage.io_worker_threads, 3);
        assert_eq!(c.storage.io_queue_depth, 77);
        assert_eq!(c.storage.hash_worker_threads, 4);
        assert_eq!(c.storage.hash_queue_depth, 88);
        assert_eq!(
            c.storage.preallocation_mode,
            StoragePreallocationMode::Sparse
        );
        assert_eq!(c.storage.durability_mode, StorageDurabilityMode::Strict);
        assert_eq!(c.storage.peer_read_readahead_bytes, 131072);
        assert_eq!(c.storage.peer_read_cache_entries, 17);
        assert_eq!(c.storage.peer_read_elevator_budget_ms, 9);
        assert_eq!(c.daemon.shutdown_timeout_secs, 10);
        assert_eq!(c.logging, rt_logging::LoggingConfig::default());
    }

    #[test]
    fn parse_logging_toml() {
        let toml = r#"
[daemon]
log_level = "warn"

[logging]
format = "pretty"
profile = "detailed"
filter = "rt_engine=debug"
event_retention = 2048
"#;
        let c: Config = toml::from_str(toml).unwrap();
        c.validate().unwrap();
        assert_eq!(c.daemon.log_level, "warn");
        assert_eq!(c.logging.format, rt_logging::LogFormat::Pretty);
        assert_eq!(c.logging.profile, rt_logging::LogProfile::Detailed);
        assert_eq!(c.logging.filter, "rt_engine=debug");
        assert_eq!(c.logging.event_retention, 2048);
    }
}
