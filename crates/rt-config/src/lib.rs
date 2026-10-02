use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

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
    /// Host-crash detection, completion gating and post-crash verification.
    /// See `docs/CRASH_SAFETY.md`.
    pub crash_safety: CrashSafetyConfig,
    pub memory: MemoryConfig,
    pub runtime: RuntimeConfig,
    pub tracker: TrackerConfig,
    pub dht: DhtConfig,
    pub db: DbConfig,
    pub auth: AuthConfig,
    pub metrics: MetricsConfig,
    pub logging: rt_logging::LoggingConfig,
}

// Tokio semaphores reserve three bits for internal bookkeeping. Keep the
// configuration boundary aligned with that runtime limit so an invalid peer
// budget is reported as a config error instead of panicking during startup.
const MAX_SEMAPHORE_PERMITS: usize = usize::MAX >> 3;
// Storage resources are process-wide but are constructed from operator-owned
// configuration. Keep malformed values from spawning an unbounded number of
// OS threads, allocating enormous channel capacities, or retaining an
// unreasonable number of open-file cache entries.
const MAX_STORAGE_WORKER_THREADS: usize = 64;
const MAX_STORAGE_QUEUE_DEPTH: usize = 16_384;
const MAX_STORAGE_FILE_POOL_SIZE: usize = 65_536;
/// Bound the per-scheduler peer-read cache metadata even when a deployment
/// disables the shared memory governor. The cache stores one path-keyed entry
/// per file and is instantiated for every active storage scheduler.
const MAX_STORAGE_PEER_READ_CACHE_ENTRIES: usize = 65_536;
/// The elevator budget is a batching window, not a timeout. A very large
/// operator value can hold HDD peer reads for hours and make the scheduler
/// appear hung, so reject values beyond one minute at the config boundary.
const MAX_STORAGE_PEER_READ_ELEVATOR_BUDGET_MS: u64 = 60_000;
const MAX_DHT_BOOTSTRAP_NODES: usize = 256;
const MAX_DHT_BOOTSTRAP_NODE_BYTES: usize = 256;
const MAX_DHT_BOOTSTRAP_BYTES: usize = 64 * 1024;
/// Default admission cap for `DhtConfig::tracked_torrents_cap`. Chosen to sit
/// comfortably above this project's stated 10k-100k torrent scale target
/// (see docs/ENGINE.md and CLAUDE.md) rather than silently stranding DHT
/// discovery for torrents beyond an old, much lower hardcoded limit.
const DEFAULT_DHT_TRACKED_TORRENTS_CAP: usize = 131_072;
/// Sanity ceiling on the configured cap. This is a safety net against a
/// misconfigured value producing runaway memory use, not a realistic
/// deployment target -- it is far above the 100k top-end scale target.
const MAX_DHT_TRACKED_TORRENTS_CAP: usize = 1_000_000;
const MAX_AUTH_TOKENS: usize = 256;
const MAX_AUTH_TOKEN_BYTES: usize = 4096;

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
    /// Maximum accepted-but-not-routed inbound peer handshakes across the daemon.
    pub max_incoming_handshakes: usize,
    /// Maximum inbound handshakes accepted from one IP during the configured window.
    pub max_incoming_handshakes_per_ip: usize,
    /// Sliding window for per-IP inbound handshake throttling.
    pub incoming_handshake_window_secs: u64,
    /// Timeout for inbound peer handshakes before routing to a torrent task.
    pub incoming_handshake_timeout_secs: u64,
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

/// What to verify for a torrent after the daemon detects that the previous
/// run ended in a host crash (power loss, kernel panic, hard reset) instead of
/// a clean shutdown or a process-only crash.
///
/// A host crash discards the OS page cache. Every mode below already benefits
/// from the durability-barrier ordering (data fsync, then fastresume); these
/// modes decide how much *additional* hashing to do as defense in depth
/// against storage that acknowledged a sync it did not perform.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostCrashRecovery {
    /// Trust the durable fastresume state and recheck only the dirty-piece
    /// watermark. Fastest; correct when storage honors fsync.
    Watermark,
    /// Watermark, plus a full recheck of every torrent whose payload was
    /// written within `recent_write_window_secs` before the crash.
    #[default]
    Recent,
    /// Full recheck of every torrent. Expensive on large libraries.
    Full,
}

/// When to compare a torrent's `Valid` pieces against the filesystem's own
/// allocation map (holes and unwritten extents) before trusting them.
///
/// The audit is a metadata-only prefilter. A `Valid` piece that overlaps a
/// hole or unwritten extent is downgraded to `Unknown` and re-hashed; it is
/// never declared corrupt from allocation alone, because compressing and
/// deduplicating filesystems legitimately store zero runs as holes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StructuralAuditMode {
    Off,
    /// Only after an unclean shutdown (host crash, process crash, or an
    /// undetectable previous run). Default.
    #[default]
    OnUnclean,
    /// On every torrent start. Costs one allocation query per file.
    Always,
}

/// Optional re-read of freshly downloaded data from disk before the torrent is
/// reported complete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CompletionVerifyMode {
    /// Rely on the durability barrier alone. Default; no extra reads.
    #[default]
    Off,
    /// After the barrier, drop cached pages where the OS allows it and re-hash
    /// a bounded sample: every file's first and last piece plus a
    /// deterministic percentage of the rest.
    Sample,
    /// After the barrier, re-hash every piece. Doubles the read I/O of a
    /// download.
    Full,
}

/// Unknown keys are rejected (config file and settings API alike): a typo in a
/// safety setting must fail loudly instead of leaving the default in force
/// while the operator believes otherwise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CrashSafetyConfig {
    /// Do not report a download as complete (progress 100%, `Seeding`, tracker
    /// `completed`, `completed_at`) until its data has been made durable by a
    /// storage barrier. While gated the torrent stays `Downloading` and
    /// reports one byte remaining, so *arr clients do not import it early.
    pub completion_gate: bool,
    /// Keep a durable run marker and the OS boot identity so a host crash can
    /// be told apart from a process crash. When disabled no marker is kept:
    /// `host_crash_recovery`, weak-mount escalation and the unsynced-state
    /// check are skipped, and recovery relies only on each torrent's own
    /// fastresume flags (the behavior before this option existed).
    pub host_crash_detection: bool,
    pub host_crash_recovery: HostCrashRecovery,
    /// Look-back window for [`HostCrashRecovery::Recent`], in seconds.
    pub recent_write_window_secs: u64,
    pub structural_audit: StructuralAuditMode,
    /// Classify the filesystem under each save path and surface it in the API.
    pub mount_probe: bool,
    /// Recover more aggressively on mounts classified weak or unknown:
    /// `Weak` mounts use a full recheck, `Unknown` mounts use at least
    /// `recent`.
    pub weak_mount_escalation: bool,
    /// Operator overrides: paths under these prefixes are always classified
    /// weak, or strong, regardless of what the probe detects.
    pub weak_mount_paths: Vec<PathBuf>,
    pub strong_mount_paths: Vec<PathBuf>,
    pub completion_verify: CompletionVerifyMode,
    /// Sample size for [`CompletionVerifyMode::Sample`], percent of pieces.
    pub completion_verify_sample_percent: u8,
    /// Per-location overrides of the settings above. The longest path prefix
    /// that contains a torrent's save path wins; anything a policy leaves
    /// unset is inherited from this section. See [`PathPolicy`].
    pub path_policies: Vec<PathPolicy>,
}

/// An override of the crash-safety behavior for torrents saved under one
/// directory: for example a lean policy for a scratch SSD and full read-back
/// for a NAS. Unset fields inherit the global value.
///
/// Only settings that make sense per location can be overridden. Host-crash
/// detection (one run marker per daemon), the recent-write window and the
/// mount probe flags stay global; mount trust per location is already
/// controlled by `weak_mount_paths` / `strong_mount_paths`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PathPolicy {
    /// Absolute directory this policy applies to (prefix match on components).
    pub path: PathBuf,
    pub completion_gate: Option<bool>,
    pub host_crash_recovery: Option<HostCrashRecovery>,
    pub structural_audit: Option<StructuralAuditMode>,
    pub completion_verify: Option<CompletionVerifyMode>,
    pub completion_verify_sample_percent: Option<u8>,
}

/// The settings that apply to one save location after resolving
/// [`PathPolicy`] overrides against the global values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCrashSafety {
    pub completion_gate: bool,
    pub host_crash_recovery: HostCrashRecovery,
    pub structural_audit: StructuralAuditMode,
    pub completion_verify: CompletionVerifyMode,
    pub completion_verify_sample_percent: u8,
}

const MAX_CRASH_SAFETY_PATHS: usize = 64;
const MAX_CRASH_SAFETY_WINDOW_SECS: u64 = 365 * 24 * 60 * 60;

impl Default for CrashSafetyConfig {
    fn default() -> Self {
        CrashSafetyConfig {
            completion_gate: true,
            host_crash_detection: true,
            host_crash_recovery: HostCrashRecovery::Recent,
            recent_write_window_secs: 24 * 60 * 60,
            structural_audit: StructuralAuditMode::OnUnclean,
            mount_probe: true,
            weak_mount_escalation: true,
            weak_mount_paths: Vec::new(),
            strong_mount_paths: Vec::new(),
            completion_verify: CompletionVerifyMode::Off,
            completion_verify_sample_percent: 5,
            path_policies: Vec::new(),
        }
    }
}

impl CrashSafetyConfig {
    /// Resolve the settings for a torrent saved at `save_path`: the
    /// [`PathPolicy`] with the longest matching path prefix overrides the
    /// global values field by field. Also returns the policy that matched.
    pub fn resolve_for(&self, save_path: &Path) -> (ResolvedCrashSafety, Option<&PathPolicy>) {
        let matched = self
            .path_policies
            .iter()
            .filter(|policy| save_path.starts_with(&policy.path))
            .max_by_key(|policy| policy.path.components().count());
        let resolved = ResolvedCrashSafety {
            completion_gate: matched
                .and_then(|p| p.completion_gate)
                .unwrap_or(self.completion_gate),
            host_crash_recovery: matched
                .and_then(|p| p.host_crash_recovery)
                .unwrap_or(self.host_crash_recovery),
            structural_audit: matched
                .and_then(|p| p.structural_audit)
                .unwrap_or(self.structural_audit),
            completion_verify: matched
                .and_then(|p| p.completion_verify)
                .unwrap_or(self.completion_verify),
            completion_verify_sample_percent: matched
                .and_then(|p| p.completion_verify_sample_percent)
                .unwrap_or(self.completion_verify_sample_percent),
        };
        (resolved, matched)
    }

    /// Range and shape checks shared by config-file loading and the runtime
    /// settings API.
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(
            self.recent_write_window_secs > 0,
            "crash_safety.recent_write_window_secs must be greater than zero",
        )?;
        require(
            self.recent_write_window_secs <= MAX_CRASH_SAFETY_WINDOW_SECS,
            format!(
                "crash_safety.recent_write_window_secs must be <= {MAX_CRASH_SAFETY_WINDOW_SECS}"
            ),
        )?;
        require(
            (1..=100).contains(&self.completion_verify_sample_percent),
            "crash_safety.completion_verify_sample_percent must be between 1 and 100",
        )?;
        for (field, paths) in [
            ("weak_mount_paths", &self.weak_mount_paths),
            ("strong_mount_paths", &self.strong_mount_paths),
        ] {
            require(
                paths.len() <= MAX_CRASH_SAFETY_PATHS,
                format!("crash_safety.{field} must contain <= {MAX_CRASH_SAFETY_PATHS} entries"),
            )?;
            for path in paths {
                require(
                    path.is_absolute(),
                    format!("crash_safety.{field} entries must be absolute paths"),
                )?;
            }
        }
        require(
            self.path_policies.len() <= MAX_CRASH_SAFETY_PATHS,
            format!("crash_safety.path_policies must contain <= {MAX_CRASH_SAFETY_PATHS} entries"),
        )?;
        for (index, policy) in self.path_policies.iter().enumerate() {
            require(
                policy.path.is_absolute(),
                format!("crash_safety.path_policies[{index}].path must be an absolute path"),
            )?;
            if let Some(percent) = policy.completion_verify_sample_percent {
                require(
                    (1..=100).contains(&percent),
                    format!(
                        "crash_safety.path_policies[{index}].completion_verify_sample_percent must be between 1 and 100"
                    ),
                )?;
            }
            require(
                !self.path_policies[..index]
                    .iter()
                    .any(|earlier| earlier.path == policy.path),
                format!(
                    "crash_safety.path_policies[{index}].path duplicates an earlier entry; each location may have one policy"
                ),
            )?;
        }
        for weak in &self.weak_mount_paths {
            require(
                !self.strong_mount_paths.iter().any(|strong| strong == weak),
                "crash_safety.weak_mount_paths and strong_mount_paths must not share an entry",
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub total_cap_mb: u64,
    pub storage_frame_cap_mb: u64,
    pub queued_disk_cap_mb: u64,
    pub piece_assembly_cap_mb: u64,
    /// Persistent per-torrent piece picker and availability index cap.
    pub piece_index_cap_mb: u64,
    pub peer_buffer_cap_mb: u64,
    pub metadata_cap_mb: u64,
    pub pressure_constrained_pct: u8,
    pub pressure_critical_pct: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub torrent_tiers_enabled: bool,
    /// Seconds a promoted torrent may remain idle before entering Warm.
    pub tier_hot_idle_secs: u64,
    /// Seconds a promoted torrent may remain idle before entering Dormant.
    pub tier_warm_idle_secs: u64,
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
    pub allow_http_trackers: bool,
    pub allow_https_trackers: bool,
    pub allow_udp_trackers: bool,
    pub allow_http_webseeds: bool,
    pub allow_https_webseeds: bool,
    /// Permit outbound tracker, webseed, DHT, and peer traffic to loopback IPs.
    pub allow_loopback_egress: bool,
    /// Permit outbound tracker, webseed, DHT, and peer traffic to private IPs.
    pub allow_private_egress: bool,
    /// Permit outbound tracker, webseed, DHT, and peer traffic to link-local IPs.
    pub allow_link_local_egress: bool,
    /// Permit outbound tracker, webseed, DHT, and peer traffic to multicast IPs.
    pub allow_multicast_egress: bool,
    /// Permit outbound tracker, webseed, DHT, and peer traffic to unspecified IPs.
    pub allow_unspecified_egress: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DhtConfig {
    pub enabled: bool,
    /// UDP port for DHT. 0 = same as listen_port.
    pub port: u16,
    /// Bootstrap nodes as "host:port" strings.
    pub bootstrap_nodes: Vec<String>,
    /// Maximum number of torrents the DHT task will concurrently track for
    /// peer discovery. Torrents beyond this cap are still transferred and
    /// tracker-announced normally; they simply do not get DHT-sourced peers.
    ///
    /// This used to be a hardcoded 16,384 constant, which silently stranded
    /// DHT discovery for the majority of torrents at this project's stated
    /// 10k-100k torrent scale target. The default here (131,072) sits
    /// comfortably above the top of that range; lower it as a safety valve
    /// on memory- or CPU-constrained deployments.
    pub tracked_torrents_cap: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DbConfig {
    /// Path to the SQLite database file. Empty = session_dir/state.db.
    pub path: PathBuf,
    /// SQLite WAL checkpoint threshold (pages).
    pub wal_checkpoint_pages: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// WebUI username. The API token remains accepted separately for API
    /// clients and can also be entered in either WebUI login field.
    pub username: String,
    /// WebUI password. Operators can change this from Settings after login;
    /// runtime changes are stored in the session directory.
    pub password: String,
    /// Pre-shared bearer/session tokens accepted by the TorrentNG API.
    pub api_tokens: Vec<String>,
    /// Optional newline-delimited token file. Values are appended to
    /// `api_tokens` while loading a config file, which lets container and
    /// systemd deployments keep secrets out of the main TOML document.
    pub api_tokens_file: Option<PathBuf>,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthConfig")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field(
                "api_tokens",
                &format_args!("[{} configured]", self.api_tokens.len()),
            )
            .field("api_tokens_file", &self.api_tokens_file)
            .finish()
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            username: "torrentng".to_owned(),
            // Empty selects a private per-install bootstrap password created
            // by torrentngd in the session directory at startup.
            password: String::new(),
            api_tokens: Vec::new(),
            api_tokens_file: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    /// Include raw torrent infohashes in per-torrent Prometheus labels.
    ///
    /// This is disabled by default because labels are high-cardinality and
    /// expose operational identifiers to every metrics reader.
    pub include_torrent_ids: bool,
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
            max_incoming_handshakes: 256,
            max_incoming_handshakes_per_ip: 16,
            incoming_handshake_window_secs: 30,
            incoming_handshake_timeout_secs: 10,
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
            piece_index_cap_mb: 128,
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
            tier_hot_idle_secs: 2 * 60,
            tier_warm_idle_secs: 30 * 60,
        }
    }
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig {
            http_timeout_secs: 30,
            udp_timeout_secs: 15,
            min_interval_secs: 0,
            allow_http_trackers: true,
            allow_https_trackers: true,
            allow_udp_trackers: true,
            allow_http_webseeds: true,
            allow_https_webseeds: true,
            allow_loopback_egress: false,
            allow_private_egress: false,
            allow_link_local_egress: false,
            allow_multicast_egress: false,
            allow_unspecified_egress: false,
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
            tracked_torrents_cap: DEFAULT_DHT_TRACKED_TORRENTS_CAP,
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
        let text = read_bounded_text(path, MAX_CONFIG_BYTES).map_err(|error| match error {
            BoundedTextError::Io(error) => ConfigError::Io(error),
            BoundedTextError::TooLarge => ConfigError::Validation(format!(
                "config {} exceeds {} bytes",
                path.display(),
                MAX_CONFIG_BYTES
            )),
            BoundedTextError::Utf8 => {
                ConfigError::Validation(format!("config {} is not valid UTF-8", path.display()))
            }
        })?;
        let mut config: Self = toml::from_str(&text)?;
        config.load_api_tokens_file(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Load from the standard search path, returning defaults only when no
    /// config file exists. An existing but invalid file is an operator error
    /// and is never silently ignored.
    pub fn load_default() -> Result<Self, ConfigError> {
        for path in default_config_paths() {
            if path.exists() {
                return Self::load(&path).map_err(|error| {
                    ConfigError::Validation(format!("{}: {error}", path.display()))
                });
            }
        }
        Ok(Self::default())
    }

    /// Validate config invariants that would otherwise turn into runtime footguns.
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(
            !self.daemon.api_bind.trim().is_empty(),
            "daemon.api_bind must not be empty",
        )?;
        let api_addr = self
            .daemon
            .api_bind
            .parse::<SocketAddr>()
            .map_err(|error| ConfigError::Validation(format!("daemon.api_bind: {error}")))?;
        require(
            self.daemon.shutdown_timeout_secs > 0,
            "daemon.shutdown_timeout_secs must be greater than zero",
        )?;
        require(
            self.network.max_peers > 0,
            "network.max_peers must be greater than zero",
        )?;
        require(
            self.network.max_peers <= MAX_SEMAPHORE_PERMITS,
            format!("network.max_peers must be <= {MAX_SEMAPHORE_PERMITS}"),
        )?;
        require(
            self.network.max_incoming_handshakes > 0,
            "network.max_incoming_handshakes must be greater than zero",
        )?;
        require(
            self.network.max_incoming_handshakes <= MAX_SEMAPHORE_PERMITS,
            format!("network.max_incoming_handshakes must be <= {MAX_SEMAPHORE_PERMITS}"),
        )?;
        require(
            self.network.max_incoming_handshakes_per_ip > 0,
            "network.max_incoming_handshakes_per_ip must be greater than zero",
        )?;
        require(
            self.network.incoming_handshake_window_secs > 0,
            "network.incoming_handshake_window_secs must be greater than zero",
        )?;
        require(
            self.network.incoming_handshake_timeout_secs > 0,
            "network.incoming_handshake_timeout_secs must be greater than zero",
        )?;
        require(
            self.runtime.tier_hot_idle_secs > 0,
            "runtime.tier_hot_idle_secs must be greater than zero",
        )?;
        require(
            self.runtime.tier_warm_idle_secs > self.runtime.tier_hot_idle_secs,
            "runtime.tier_warm_idle_secs must be greater than runtime.tier_hot_idle_secs",
        )?;
        require(
            !self.storage.download_dir.as_os_str().is_empty(),
            "storage.download_dir must not be empty",
        )?;
        require(
            self.storage.file_pool_size > 0,
            "storage.file_pool_size must be greater than zero",
        )?;
        require(
            self.storage.file_pool_size <= MAX_STORAGE_FILE_POOL_SIZE,
            format!("storage.file_pool_size must be <= {MAX_STORAGE_FILE_POOL_SIZE}"),
        )?;
        require(
            self.storage.io_worker_threads > 0,
            "storage.io_worker_threads must be greater than zero",
        )?;
        require(
            self.storage.io_worker_threads <= MAX_STORAGE_WORKER_THREADS,
            format!("storage.io_worker_threads must be <= {MAX_STORAGE_WORKER_THREADS}"),
        )?;
        require(
            self.storage.io_queue_depth > 0,
            "storage.io_queue_depth must be greater than zero",
        )?;
        require(
            self.storage.io_queue_depth <= MAX_STORAGE_QUEUE_DEPTH,
            format!("storage.io_queue_depth must be <= {MAX_STORAGE_QUEUE_DEPTH}"),
        )?;
        require(
            self.storage.hash_worker_threads > 0,
            "storage.hash_worker_threads must be greater than zero",
        )?;
        require(
            self.storage.hash_worker_threads <= MAX_STORAGE_WORKER_THREADS,
            format!("storage.hash_worker_threads must be <= {MAX_STORAGE_WORKER_THREADS}"),
        )?;
        require(
            self.storage.hash_queue_depth > 0,
            "storage.hash_queue_depth must be greater than zero",
        )?;
        require(
            self.storage.hash_queue_depth <= MAX_STORAGE_QUEUE_DEPTH,
            format!("storage.hash_queue_depth must be <= {MAX_STORAGE_QUEUE_DEPTH}"),
        )?;
        require(
            self.storage.peer_read_readahead_bytes <= 64 * 1024 * 1024,
            "storage.peer_read_readahead_bytes must be <= 64MiB",
        )?;
        require(
            self.storage.peer_read_cache_entries <= MAX_STORAGE_PEER_READ_CACHE_ENTRIES,
            format!(
                "storage.peer_read_cache_entries must be <= {MAX_STORAGE_PEER_READ_CACHE_ENTRIES}"
            ),
        )?;
        require(
            self.storage.peer_read_elevator_budget_ms
                <= MAX_STORAGE_PEER_READ_ELEVATOR_BUDGET_MS,
            format!(
                "storage.peer_read_elevator_budget_ms must be <= {MAX_STORAGE_PEER_READ_ELEVATOR_BUDGET_MS}"
            ),
        )?;
        self.crash_safety.validate()?;
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
            (
                "memory.storage_frame_cap_mb",
                self.memory.storage_frame_cap_mb,
            ),
            ("memory.queued_disk_cap_mb", self.memory.queued_disk_cap_mb),
            (
                "memory.piece_assembly_cap_mb",
                self.memory.piece_assembly_cap_mb,
            ),
            ("memory.piece_index_cap_mb", self.memory.piece_index_cap_mb),
            ("memory.peer_buffer_cap_mb", self.memory.peer_buffer_cap_mb),
            ("memory.metadata_cap_mb", self.memory.metadata_cap_mb),
        ] {
            require(
                value <= self.memory.total_cap_mb,
                format!("{field} must be <= memory.total_cap_mb"),
            )?;
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
            self.tracker.allow_http_trackers
                || self.tracker.allow_https_trackers
                || self.tracker.allow_udp_trackers,
            "at least one tracker scheme must be enabled",
        )?;
        require(
            self.tracker.allow_http_webseeds || self.tracker.allow_https_webseeds,
            "at least one webseed scheme must be enabled",
        )?;
        require(
            self.dht.bootstrap_nodes.len() <= MAX_DHT_BOOTSTRAP_NODES,
            format!("dht.bootstrap_nodes must contain <= {MAX_DHT_BOOTSTRAP_NODES} entries"),
        )?;
        let mut bootstrap_bytes = 0usize;
        for (index, node) in self.dht.bootstrap_nodes.iter().enumerate() {
            let node = node.trim();
            require(
                !node.is_empty(),
                format!("dht.bootstrap_nodes[{index}] must not be empty"),
            )?;
            require(
                node.len() <= MAX_DHT_BOOTSTRAP_NODE_BYTES,
                format!(
                    "dht.bootstrap_nodes[{index}] must be <= {MAX_DHT_BOOTSTRAP_NODE_BYTES} bytes"
                ),
            )?;
            bootstrap_bytes = bootstrap_bytes.saturating_add(node.len());
        }
        require(
            bootstrap_bytes <= MAX_DHT_BOOTSTRAP_BYTES,
            format!("dht.bootstrap_nodes must contain <= {MAX_DHT_BOOTSTRAP_BYTES} bytes"),
        )?;
        require(
            self.dht.tracked_torrents_cap > 0,
            "dht.tracked_torrents_cap must be greater than zero",
        )?;
        require(
            self.dht.tracked_torrents_cap <= MAX_DHT_TRACKED_TORRENTS_CAP,
            format!("dht.tracked_torrents_cap must be <= {MAX_DHT_TRACKED_TORRENTS_CAP}"),
        )?;
        require(
            self.db.wal_checkpoint_pages > 0,
            "db.wal_checkpoint_pages must be greater than zero",
        )?;
        require(
            !self.auth.username.trim().is_empty() && self.auth.username.len() <= 256,
            "auth.username must contain 1-256 bytes",
        )?;
        require(
            self.auth.password.is_empty()
                || (self.auth.password.trim().len() >= 8 && self.auth.password.len() <= 1024),
            "auth.password must be empty for a generated password or contain 8-1024 bytes",
        )?;
        require(
            self.auth.api_tokens.len() <= MAX_AUTH_TOKENS,
            format!("auth.api_tokens must contain <= {MAX_AUTH_TOKENS} tokens"),
        )?;
        for token in &self.auth.api_tokens {
            require(
                !token.trim().is_empty(),
                "auth.api_tokens must not contain empty tokens",
            )?;
            require(
                token.len() <= MAX_AUTH_TOKEN_BYTES,
                format!("auth.api_tokens entries must be <= {MAX_AUTH_TOKEN_BYTES} bytes"),
            )?;
            require(
                !is_placeholder_token(token),
                "auth.api_tokens must not contain a placeholder token",
            )?;
        }
        if !api_addr.ip().is_loopback() {
            require(
                !self.auth.api_tokens.is_empty(),
                "public daemon.api_bind requires at least one API token",
            )?;
            require(
                self.auth
                    .api_tokens
                    .iter()
                    .all(|token| token.trim().len() >= 16),
                "public daemon.api_bind requires API tokens of at least 16 characters",
            )?;
        }
        Ok(())
    }

    /// Resolved DB path (falls back to session_dir/state.db).
    pub fn db_path(&self) -> PathBuf {
        if self.db.path == PathBuf::new() {
            self.daemon.session_dir.join("state.db")
        } else if self.db.path.is_absolute() {
            self.db.path.clone()
        } else {
            self.daemon.session_dir.join(&self.db.path)
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

    fn load_api_tokens_file(&mut self, config_path: &Path) -> Result<(), ConfigError> {
        let Some(token_path) = self.auth.api_tokens_file.as_ref() else {
            return Ok(());
        };
        let token_path = if token_path.is_absolute() {
            token_path.clone()
        } else {
            config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(token_path)
        };
        let token_text = read_bounded_text(&token_path, MAX_CONFIG_BYTES).map_err(|error| {
            ConfigError::Validation(format!(
                "auth.api_tokens_file {}: {error}",
                token_path.display()
            ))
        })?;
        let max_file_tokens = MAX_AUTH_TOKENS.saturating_sub(self.auth.api_tokens.len());
        let mut file_tokens = Vec::new();
        for token in token_text
            .lines()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            if file_tokens.len() >= max_file_tokens {
                return Err(ConfigError::Validation(format!(
                    "auth.api_tokens_file {} contains more than {} tokens",
                    token_path.display(),
                    MAX_AUTH_TOKENS
                )));
            }
            if token.len() > MAX_AUTH_TOKEN_BYTES {
                return Err(ConfigError::Validation(format!(
                    "auth.api_tokens_file {} contains a token longer than {} bytes",
                    token_path.display(),
                    MAX_AUTH_TOKEN_BYTES
                )));
            }
            file_tokens.push(token.to_owned());
        }
        if file_tokens.is_empty() {
            return Err(ConfigError::Validation(format!(
                "auth.api_tokens_file {} contains no tokens",
                token_path.display()
            )));
        }
        self.auth.api_tokens.extend(file_tokens);
        Ok(())
    }
}

enum BoundedTextError {
    Io(std::io::Error),
    TooLarge,
    Utf8,
}

impl std::fmt::Display for BoundedTextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::TooLarge => write!(f, "file exceeds configured size limit"),
            Self::Utf8 => write!(f, "file is not valid UTF-8"),
        }
    }
}

fn read_bounded_text(path: &Path, max_bytes: u64) -> Result<String, BoundedTextError> {
    let file = File::open(path).map_err(BoundedTextError::Io)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(BoundedTextError::Io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(BoundedTextError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| BoundedTextError::Utf8)
}

fn require(condition: bool, message: impl Into<String>) -> Result<(), ConfigError> {
    if condition {
        Ok(())
    } else {
        Err(ConfigError::Validation(message.into()))
    }
}

fn is_placeholder_token(token: &str) -> bool {
    matches!(
        token.trim().to_ascii_lowercase().as_str(),
        "change-me" | "changeme" | "replace-me" | "replace_with_random_token"
    ) || token.trim().to_ascii_uppercase().contains("REPLACE_WITH")
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
        assert_eq!(c.network.max_incoming_handshakes, 256);
        assert_eq!(c.network.max_incoming_handshakes_per_ip, 16);
        assert_eq!(c.network.incoming_handshake_window_secs, 30);
        assert_eq!(c.network.incoming_handshake_timeout_secs, 10);
        assert!(c.dht.enabled);
        assert!(!c.dht.bootstrap_nodes.is_empty());
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.tracker.allow_http_trackers);
        assert!(c.tracker.allow_https_trackers);
        assert!(c.tracker.allow_udp_trackers);
        assert!(c.tracker.allow_http_webseeds);
        assert!(c.tracker.allow_https_webseeds);
        assert!(!c.tracker.allow_loopback_egress);
        assert!(!c.tracker.allow_private_egress);
        assert!(!c.tracker.allow_link_local_egress);
        assert!(c.auth.api_tokens.is_empty());
        assert!(c.auth.api_tokens_file.is_none());
        assert_eq!(c.auth.username, "torrentng");
        assert!(c.auth.password.is_empty());
        assert_eq!(c.daemon.shutdown_timeout_secs, 10);
        assert_eq!(c.memory.total_cap_mb, 512);
        assert_eq!(c.memory.storage_frame_cap_mb, 128);
        assert_eq!(c.memory.queued_disk_cap_mb, 64);
        assert_eq!(c.memory.piece_index_cap_mb, 128);
        assert!(c.runtime.torrent_tiers_enabled);
        assert_eq!(c.runtime.tier_hot_idle_secs, 120);
        assert_eq!(c.runtime.tier_warm_idle_secs, 1_800);
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

    /// An absolute path on the platform running the tests: a leading `/` is not
    /// absolute on Windows, which needs a drive.
    fn abs(name: &str) -> String {
        if cfg!(windows) {
            format!("C:/{name}")
        } else {
            format!("/mnt/{name}")
        }
    }

    #[test]
    fn crash_safety_defaults_are_safe_and_valid() {
        let c = Config::default();
        assert!(c.crash_safety.completion_gate);
        assert!(c.crash_safety.host_crash_detection);
        assert_eq!(
            c.crash_safety.host_crash_recovery,
            HostCrashRecovery::Recent
        );
        assert_eq!(c.crash_safety.recent_write_window_secs, 86_400);
        assert_eq!(
            c.crash_safety.structural_audit,
            StructuralAuditMode::OnUnclean
        );
        assert!(c.crash_safety.mount_probe);
        assert!(c.crash_safety.weak_mount_escalation);
        assert_eq!(c.crash_safety.completion_verify, CompletionVerifyMode::Off);
        c.crash_safety.validate().unwrap();
    }

    #[test]
    fn crash_safety_parses_from_toml_and_defaults_missing_fields() {
        let c: Config = toml::from_str(&format!(
            r#"
[crash_safety]
host_crash_recovery = "full"
structural_audit = "always"
completion_verify = "sample"
completion_verify_sample_percent = 10
weak_mount_paths = ["{}"]
"#,
            abs("pool")
        ))
        .unwrap();
        c.validate().unwrap();
        assert_eq!(c.crash_safety.host_crash_recovery, HostCrashRecovery::Full);
        assert_eq!(c.crash_safety.structural_audit, StructuralAuditMode::Always);
        assert_eq!(
            c.crash_safety.completion_verify,
            CompletionVerifyMode::Sample
        );
        assert_eq!(c.crash_safety.completion_verify_sample_percent, 10);
        assert_eq!(
            c.crash_safety.weak_mount_paths,
            vec![PathBuf::from(abs("pool"))]
        );
        // Untouched fields keep their defaults.
        assert!(c.crash_safety.completion_gate);
        assert_eq!(c.crash_safety.recent_write_window_secs, 86_400);
    }

    #[test]
    fn crash_safety_rejects_out_of_range_and_conflicting_values() {
        let base = CrashSafetyConfig::default;

        let c = CrashSafetyConfig {
            recent_write_window_secs: 0,
            ..base()
        };
        assert!(c.validate().is_err());

        let c = CrashSafetyConfig {
            recent_write_window_secs: MAX_CRASH_SAFETY_WINDOW_SECS + 1,
            ..base()
        };
        assert!(c.validate().is_err());

        for pct in [0u8, 101] {
            let c = CrashSafetyConfig {
                completion_verify_sample_percent: pct,
                ..base()
            };
            assert!(c.validate().is_err(), "{pct}% must be rejected");
        }

        let c = CrashSafetyConfig {
            weak_mount_paths: vec![PathBuf::from("relative/path")],
            ..base()
        };
        assert!(c.validate().is_err());

        let c = CrashSafetyConfig {
            weak_mount_paths: vec![PathBuf::from(abs("a"))],
            strong_mount_paths: vec![PathBuf::from(abs("a"))],
            ..base()
        };
        assert!(c.validate().is_err());

        let c = CrashSafetyConfig {
            weak_mount_paths: (0..=MAX_CRASH_SAFETY_PATHS)
                .map(|i| PathBuf::from(abs(&format!("d{i}"))))
                .collect(),
            ..base()
        };
        assert!(c.validate().is_err());
    }

    fn policy(path: &str) -> PathPolicy {
        PathPolicy {
            path: PathBuf::from(abs(path)),
            ..PathPolicy::default()
        }
    }

    #[test]
    fn path_policies_resolve_by_longest_prefix_and_inherit_unset_fields() {
        let config = CrashSafetyConfig {
            completion_verify: CompletionVerifyMode::Sample,
            path_policies: vec![
                PathPolicy {
                    completion_gate: Some(false),
                    completion_verify: Some(CompletionVerifyMode::Off),
                    ..policy("scratch")
                },
                PathPolicy {
                    host_crash_recovery: Some(HostCrashRecovery::Full),
                    completion_verify: Some(CompletionVerifyMode::Full),
                    ..policy("nas")
                },
                PathPolicy {
                    completion_verify_sample_percent: Some(50),
                    ..PathPolicy {
                        path: PathBuf::from(abs("nas")).join("media"),
                        ..PathPolicy::default()
                    }
                },
            ],
            ..CrashSafetyConfig::default()
        };
        let under = |p: &str, rest: &str| PathBuf::from(abs(p)).join(rest);

        // No policy: the global values.
        let (plain, matched) = config.resolve_for(&under("other", "x"));
        assert!(matched.is_none());
        assert!(plain.completion_gate);
        assert_eq!(plain.completion_verify, CompletionVerifyMode::Sample);
        assert_eq!(plain.host_crash_recovery, HostCrashRecovery::Recent);

        // A policy overrides only what it sets.
        let (scratch, matched) = config.resolve_for(&under("scratch", "dl/movie"));
        assert!(matched.is_some());
        assert!(!scratch.completion_gate);
        assert_eq!(scratch.completion_verify, CompletionVerifyMode::Off);
        assert_eq!(
            scratch.host_crash_recovery,
            HostCrashRecovery::Recent,
            "inherited"
        );
        assert_eq!(scratch.completion_verify_sample_percent, 5, "inherited");

        // The longest matching prefix wins outright: `nas/media` does not also
        // pick up `nas`'s settings, it inherits from the global section.
        let (nas, _) = config.resolve_for(&under("nas", "iso"));
        assert_eq!(nas.completion_verify, CompletionVerifyMode::Full);
        assert_eq!(nas.host_crash_recovery, HostCrashRecovery::Full);
        let (media, matched) = config.resolve_for(&under("nas", "media/tv"));
        assert_eq!(
            matched.unwrap().path,
            PathBuf::from(abs("nas")).join("media")
        );
        assert_eq!(media.completion_verify_sample_percent, 50);
        assert_eq!(
            media.completion_verify,
            CompletionVerifyMode::Sample,
            "not nas's Full"
        );
        assert_eq!(media.host_crash_recovery, HostCrashRecovery::Recent);

        // Matching is by path component, never by string prefix.
        let (lookalike, matched) = config.resolve_for(&PathBuf::from(abs("scratchpad")).join("x"));
        assert!(matched.is_none(), "{lookalike:?}");
    }

    #[test]
    fn path_policies_parse_from_toml_array_of_tables() {
        let c: Config = toml::from_str(&format!(
            r#"
[crash_safety]
completion_verify = "off"

[[crash_safety.path_policies]]
path = "{a}"
completion_verify = "full"
host_crash_recovery = "full"

[[crash_safety.path_policies]]
path = "{b}"
completion_gate = false
"#,
            a = abs("nas"),
            b = abs("scratch")
        ))
        .unwrap();
        c.validate().unwrap();
        assert_eq!(c.crash_safety.path_policies.len(), 2);
        assert_eq!(
            c.crash_safety.path_policies[0].completion_verify,
            Some(CompletionVerifyMode::Full)
        );
        assert_eq!(c.crash_safety.path_policies[1].completion_gate, Some(false));
        assert_eq!(c.crash_safety.path_policies[1].completion_verify, None);
    }

    #[test]
    fn path_policies_reject_bad_entries() {
        let with = |policies: Vec<PathPolicy>| CrashSafetyConfig {
            path_policies: policies,
            ..CrashSafetyConfig::default()
        };
        // Relative path.
        assert!(with(vec![PathPolicy {
            path: PathBuf::from("relative"),
            ..PathPolicy::default()
        }])
        .validate()
        .is_err());
        // Empty path (the Default) is not absolute either.
        assert!(with(vec![PathPolicy::default()]).validate().is_err());
        // Out-of-range sample percent.
        for pct in [0u8, 101] {
            assert!(with(vec![PathPolicy {
                completion_verify_sample_percent: Some(pct),
                ..policy("a")
            }])
            .validate()
            .is_err());
        }
        // Duplicate path.
        assert!(with(vec![policy("a"), policy("a")]).validate().is_err());
        // Too many.
        assert!(with(
            (0..=MAX_CRASH_SAFETY_PATHS)
                .map(|i| policy(&format!("p{i}")))
                .collect()
        )
        .validate()
        .is_err());
        // Fine.
        with(vec![policy("a"), policy("b")]).validate().unwrap();
    }

    #[test]
    fn path_policy_typos_fail_loudly() {
        let err = toml::from_str::<Config>(
            r#"
[[crash_safety.path_policies]]
path = "/x"
completion_verfy = "full"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("completion_verfy"), "{err}");
    }

    #[test]
    fn path_policies_round_trip_as_json_for_the_settings_api() {
        let c = CrashSafetyConfig {
            path_policies: vec![PathPolicy {
                completion_verify: Some(CompletionVerifyMode::Sample),
                completion_verify_sample_percent: Some(10),
                ..policy("nas")
            }],
            ..CrashSafetyConfig::default()
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["path_policies"][0]["completion_verify"], "sample");
        assert_eq!(
            json["path_policies"][0]["host_crash_recovery"],
            serde_json::Value::Null
        );
        let back: CrashSafetyConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn crash_safety_rejects_unknown_keys_so_typos_fail_loudly() {
        let err = toml::from_str::<Config>(
            r#"
[crash_safety]
host_crash_recovry = "full"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("host_crash_recovry"), "{err}");
        assert!(serde_json::from_str::<CrashSafetyConfig>(r#"{"nope": 1}"#).is_err());
    }

    #[test]
    fn crash_safety_json_round_trips_for_runtime_settings_api() {
        let c = CrashSafetyConfig {
            host_crash_recovery: HostCrashRecovery::Watermark,
            completion_verify: CompletionVerifyMode::Full,
            ..CrashSafetyConfig::default()
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: CrashSafetyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
        assert!(json.contains("\"host_crash_recovery\":\"watermark\""));
    }

    #[test]
    fn auth_config_debug_redacts_password_and_api_tokens() {
        let auth = AuthConfig {
            password: "private-password-value".to_owned(),
            api_tokens: vec!["private-api-token-value".to_owned()],
            ..AuthConfig::default()
        };

        let debug = format!("{auth:?}");
        assert!(!debug.contains("private-password-value"));
        assert!(!debug.contains("private-api-token-value"));
        assert!(debug.contains("[REDACTED]"));
        assert!(debug.contains("[1 configured]"));
    }

    #[test]
    fn db_path_fallback() {
        let c = Config::default();
        let p = c.db_path();
        assert!(p.ends_with("state.db"));
    }

    #[test]
    fn relative_db_path_resolves_under_session_directory() {
        let mut config = Config::default();
        config.daemon.session_dir = PathBuf::from("/tmp/torrentng-session");
        config.db.path = PathBuf::from("custom/state.db");
        assert_eq!(
            config.db_path(),
            PathBuf::from("/tmp/torrentng-session/custom/state.db")
        );
    }

    #[test]
    fn absolute_db_path_remains_absolute() {
        let mut config = Config::default();
        config.daemon.session_dir = PathBuf::from("/tmp/torrentng-session");
        config.db.path = PathBuf::from("/var/lib/torrentng/state.db");
        assert_eq!(
            config.db_path(),
            PathBuf::from("/var/lib/torrentng/state.db")
        );
    }

    #[test]
    fn dht_tracked_torrents_cap_default_covers_stated_scale_target() {
        // This project's own stated scale target is 10k-100k torrents (see
        // CLAUDE.md / docs/ENGINE.md). The default DHT admission cap must not
        // silently strand DHT discovery for torrents at the top of that
        // range, as the old hardcoded 16,384 constant did.
        let c = Config::default();
        assert!(c.dht.tracked_torrents_cap >= 100_000);
        assert!(c.validate().is_ok());
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

        let mut c = Config::default();
        c.auth.api_tokens = vec!["x".repeat(MAX_AUTH_TOKEN_BYTES + 1)];
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.auth.api_tokens = (0..=MAX_AUTH_TOKENS)
            .map(|index| format!("token-{index:03}-long-enough"))
            .collect();
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.network.max_incoming_handshakes = 0;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.network.max_peers = usize::MAX;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.network.max_incoming_handshakes = usize::MAX;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.io_queue_depth = usize::MAX;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.hash_queue_depth = usize::MAX;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.io_worker_threads = MAX_STORAGE_WORKER_THREADS + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.hash_worker_threads = MAX_STORAGE_WORKER_THREADS + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.file_pool_size = MAX_STORAGE_FILE_POOL_SIZE + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.peer_read_cache_entries = MAX_STORAGE_PEER_READ_CACHE_ENTRIES + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.storage.peer_read_elevator_budget_ms = MAX_STORAGE_PEER_READ_ELEVATOR_BUDGET_MS + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.dht.tracked_torrents_cap = 0;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.dht.tracked_torrents_cap = MAX_DHT_TRACKED_TORRENTS_CAP + 1;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.runtime.tier_hot_idle_secs = 0;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.runtime.tier_warm_idle_secs = c.runtime.tier_hot_idle_secs;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.daemon.api_bind = "0.0.0.0:8080".to_owned();
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.daemon.api_bind = "0.0.0.0:8080".to_owned();
        c.auth.api_tokens = vec!["REPLACE_WITH_RANDOM_TOKEN".to_owned()];
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.daemon.api_bind = "0.0.0.0:8080".to_owned();
        c.auth.api_tokens = vec!["short-token".to_owned()];
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        c.auth.api_tokens = vec!["a-real-random-token-1234".to_owned()];
        c.validate().unwrap();

        let mut c = Config::default();
        c.tracker.allow_http_trackers = false;
        c.tracker.allow_https_trackers = false;
        c.tracker.allow_udp_trackers = false;
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));

        let mut c = Config::default();
        c.dht.bootstrap_nodes = vec!["x".repeat(MAX_DHT_BOOTSTRAP_NODE_BYTES + 1)];
        assert!(matches!(c.validate(), Err(ConfigError::Validation(_))));
    }

    #[test]
    fn parse_toml_partial() {
        let toml = r#"
[network]
listen_port = 51413
max_peers = 500
max_incoming_handshakes = 128
max_incoming_handshakes_per_ip = 8
incoming_handshake_window_secs = 20
incoming_handshake_timeout_secs = 5

[auth]
api_tokens = ["one", "two"]

[tracker]
allow_private_egress = true
allow_link_local_egress = true

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
        assert_eq!(c.network.max_incoming_handshakes, 128);
        assert_eq!(c.network.max_incoming_handshakes_per_ip, 8);
        assert_eq!(c.network.incoming_handshake_window_secs, 20);
        assert_eq!(c.network.incoming_handshake_timeout_secs, 5);
        // defaults preserved for unset fields
        assert_eq!(c.tracker.http_timeout_secs, 30);
        assert!(c.tracker.allow_private_egress);
        assert!(c.tracker.allow_link_local_egress);
        assert!(c.dht.enabled);
        assert_eq!(c.auth.api_tokens, vec!["one", "two"]);
        assert!(!c.metrics.include_torrent_ids);
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
    fn metrics_torrent_identifier_opt_in_round_trips() {
        let c: Config = toml::from_str("[metrics]\ninclude_torrent_ids = true\n").unwrap();
        assert!(c.metrics.include_torrent_ids);
        c.validate().unwrap();
    }

    #[test]
    fn load_appends_newline_delimited_tokens_from_a_relative_secret_file() {
        let unique = format!(
            "torrentng-config-token-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.toml");
        std::fs::write(
            root.join("api-token"),
            "file-token-123456\nfile-token-789012\n",
        )
        .unwrap();
        std::fs::write(
            &config_path,
            "[auth]\napi_tokens = [\"inline-token-123456\"]\napi_tokens_file = \"api-token\"\n",
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.auth.api_tokens,
            vec![
                "inline-token-123456",
                "file-token-123456",
                "file-token-789012"
            ]
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_file_size_is_bounded() {
        let root = std::env::temp_dir().join(format!(
            "torrentng-config-size-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        std::fs::write(&path, vec![b' '; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        assert!(Config::load(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
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
