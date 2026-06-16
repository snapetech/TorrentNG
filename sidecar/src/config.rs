use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

/// Default user-agent announced to trackers via rTorrent.
///
/// Keep this paired with DEFAULT_PEER_ID. Do not strip it to
/// "rtorrent/0.16.11"; rTorrent 0.16.11 appends libtorrent's 0.16.11 version.
pub const DEFAULT_USER_AGENT: &str = "rtorrent/0.16.11/0.16.11";

/// Default rTorrent peer ID written to rTorrent download local_id values.
///
/// This is libtorrent 0.16.11's PEER_NAME prefix with deterministic padding.
/// Do not replace it with "rtorrent/0.16.11/000" or a guessed "-lt1011-"
/// prefix. It must be exactly 20 ASCII bytes for BitTorrent and trackers.
pub const DEFAULT_PEER_ID: &str = "-lt100B-000000000000";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,

    #[serde(default)]
    pub debug: bool,

    #[serde(default = "default_sync_interval_secs")]
    pub sync_interval_secs: u64,

    #[serde(default)]
    pub data_dir: Option<PathBuf>,

    #[serde(default)]
    pub storage_roots: Vec<PathBuf>,

    #[serde(default)]
    pub backend: BackendConfig,

    #[serde(default)]
    pub rtorrent: RtorrentConfig,

    #[serde(default)]
    pub qbittorrent: QbittorrentConfig,

    #[serde(default)]
    pub transmission: TransmissionConfig,

    #[serde(default)]
    pub deluge: DelugeConfig,

    #[serde(default)]
    pub torrentng: TorrentngConfig,

    #[serde(default)]
    pub auth: AuthConfig,

    #[serde(default)]
    pub workflows: WorkflowConfig,

    #[serde(default)]
    pub identity: IdentityConfig,

    #[serde(default)]
    pub logging: rt_logging::LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RtorrentConfig {
    /// Path to rTorrent SCGI Unix socket.
    /// Mutually exclusive with scgi_addr.
    pub scgi_socket: Option<String>,

    /// host:port for TCP SCGI (trusted local only).
    pub scgi_addr: Option<String>,

    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// User-agent string pushed to rTorrent's network.http.user_agent on startup.
    /// Override with TNG_USER_AGENT env var.
    /// See docs/TRACKER-IDENTITY.md before changing the default pair.
    #[serde(default = "default_user_agent")]
    pub user_agent: String,

    /// Peer ID pushed to rTorrent download local_id values on startup.
    /// Override with TNG_PEER_ID env var.
    /// See docs/TRACKER-IDENTITY.md before changing the default pair.
    #[serde(default = "default_peer_id")]
    pub peer_id: String,

    #[serde(default)]
    pub logs: RtorrentLogConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackendConfig {
    #[serde(rename = "type")]
    pub backend_type: BackendKind,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Rtorrent,
    Qbittorrent,
    Transmission,
    Deluge,
    Torrentng,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct QbittorrentConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub timeout_secs: u64,
    pub no_auth: bool,
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct TransmissionConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub timeout_secs: u64,
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct DelugeConfig {
    pub url: String,
    pub password: Option<String>,
    pub timeout_secs: u64,
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct TorrentngConfig {
    pub url: String,
    pub api_token: Option<String>,
    pub timeout_secs: u64,
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RtorrentLogConfig {
    pub enabled: bool,
    pub paths: Vec<PathBuf>,
    pub poll_interval_secs: u64,
    pub read_from_start: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthConfig {
    pub secret_key: Option<String>,
    pub api_tokens: Vec<String>,
    pub trust_proxy_header: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorkflowConfig {
    pub allow_scripts: bool,
    pub script_timeout_secs: u64,
    pub allowed_script_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct IdentityConfig {
    pub qbittorrent_version: String,
    pub qbittorrent_webapi_version: String,
    pub qbittorrent_build_libtorrent: String,
    pub qbittorrent_build_qt: String,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            qbittorrent_version: "5.0.0".to_owned(),
            qbittorrent_webapi_version: "2.11.0".to_owned(),
            qbittorrent_build_libtorrent: "0.16.11".to_owned(),
            qbittorrent_build_qt: "6.7.0".to_owned(),
        }
    }
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            allow_scripts: false,
            script_timeout_secs: 30,
            allowed_script_dirs: Vec::new(),
        }
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            backend_type: BackendKind::Rtorrent,
        }
    }
}

impl Default for RtorrentConfig {
    fn default() -> Self {
        Self {
            scgi_socket: Some("/run/rtorrent/rpc.sock".to_owned()),
            scgi_addr: None,
            timeout_secs: default_timeout_secs(),
            user_agent: default_user_agent(),
            peer_id: default_peer_id(),
            logs: RtorrentLogConfig::default(),
        }
    }
}

impl Default for QbittorrentConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8080".to_owned(),
            username: None,
            password: None,
            timeout_secs: default_timeout_secs(),
            no_auth: false,
            accept_invalid_certs: false,
        }
    }
}

impl Default for TransmissionConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:9091/transmission/rpc".to_owned(),
            username: None,
            password: None,
            timeout_secs: default_timeout_secs(),
            accept_invalid_certs: false,
        }
    }
}

impl Default for DelugeConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8112/json".to_owned(),
            password: None,
            timeout_secs: default_timeout_secs(),
            accept_invalid_certs: false,
        }
    }
}

impl Default for TorrentngConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8080".to_owned(),
            api_token: None,
            timeout_secs: default_timeout_secs(),
            accept_invalid_certs: false,
        }
    }
}

impl Config {
    /// Minimal config for unit/integration tests — no real rTorrent connection.
    pub fn test_default() -> Self {
        Self {
            listen_addr: "127.0.0.1:0".into(),
            debug: false,
            sync_interval_secs: 60,
            data_dir: None,
            storage_roots: vec![PathBuf::from("/")],
            backend: BackendConfig::default(),
            rtorrent: RtorrentConfig {
                scgi_socket: Some("/nonexistent".into()),
                scgi_addr: None,
                timeout_secs: 1,
                user_agent: DEFAULT_USER_AGENT.to_owned(),
                peer_id: DEFAULT_PEER_ID.to_owned(),
                logs: RtorrentLogConfig::default(),
            },
            qbittorrent: QbittorrentConfig::default(),
            transmission: TransmissionConfig::default(),
            deluge: DelugeConfig::default(),
            torrentng: TorrentngConfig::default(),
            auth: AuthConfig::default(),
            workflows: WorkflowConfig::default(),
            identity: IdentityConfig::default(),
            logging: rt_logging::LoggingConfig::default(),
        }
    }
}

// --- defaults ---

fn default_listen_addr() -> String {
    "0.0.0.0:8080".into()
}
fn default_sync_interval_secs() -> u64 {
    2
}
fn default_timeout_secs() -> u64 {
    10
}
fn default_user_agent() -> String {
    DEFAULT_USER_AGENT.to_owned()
}
fn default_peer_id() -> String {
    DEFAULT_PEER_ID.to_owned()
}

// --- loading ---

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let home = dirs_next::home_dir().context("cannot determine home directory")?;
                home.join(".config").join("torrentng").join("config.toml")
            }
        };

        let mut cfg: Config = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read config {}", path.display()))?;
            toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?
        } else {
            // No config file — synthesize a minimal default.
            // rtorrent.scgi_socket is required; will be caught in validate().
            toml::from_str(
                r#"
                [rtorrent]
                scgi_socket = "/run/rtorrent/rpc.sock"
            "#,
            )?
        };

        // Env overrides — highest priority.
        cfg.apply_env();

        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("TNG_LISTEN_ADDR") {
            self.listen_addr = v;
        }
        if let Ok(v) = std::env::var("TNG_DEBUG") {
            self.debug = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("TNG_LOG_FORMAT") {
            self.logging.format = if v.eq_ignore_ascii_case("pretty") {
                rt_logging::LogFormat::Pretty
            } else {
                rt_logging::LogFormat::Json
            };
        }
        if let Ok(v) = std::env::var("TNG_LOG_PROFILE") {
            self.logging.profile = match v.to_ascii_lowercase().as_str() {
                "detailed" => rt_logging::LogProfile::Detailed,
                "verbose" => rt_logging::LogProfile::Verbose,
                _ => rt_logging::LogProfile::Basic,
            };
        }
        if let Ok(v) = std::env::var("TNG_LOG_FILTER") {
            self.logging.filter = v;
        }
        if let Ok(v) = std::env::var("TNG_LOG_EVENT_RETENTION") {
            if let Ok(retention) = v.parse() {
                self.logging.event_retention = retention;
            }
        }
        if let Ok(v) = std::env::var("TNG_SYNC_INTERVAL_SECS") {
            if let Ok(secs) = v.parse() {
                self.sync_interval_secs = secs;
            }
        }
        if let Ok(v) = std::env::var("TNG_DATA_DIR") {
            self.data_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("TNG_USER_AGENT").or_else(|_| std::env::var("RTNG_USER_AGENT"))
        {
            self.rtorrent.user_agent = v;
        }
        if let Ok(v) = std::env::var("TNG_PEER_ID").or_else(|_| std::env::var("RTNG_PEER_ID")) {
            self.rtorrent.peer_id = v;
        }
        if let Ok(v) = std::env::var("TNG_BACKEND") {
            self.backend.backend_type = match v.to_ascii_lowercase().as_str() {
                "qbittorrent" | "qbit" => BackendKind::Qbittorrent,
                "transmission" => BackendKind::Transmission,
                "deluge" => BackendKind::Deluge,
                "torrentng" | "native" => BackendKind::Torrentng,
                _ => BackendKind::Rtorrent,
            };
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_URL") {
            self.qbittorrent.url = v;
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_USERNAME") {
            self.qbittorrent.username = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_PASSWORD") {
            self.qbittorrent.password = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_NO_AUTH") {
            self.qbittorrent.no_auth = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("TNG_TRANSMISSION_URL") {
            self.transmission.url = v;
        }
        if let Ok(v) = std::env::var("TNG_TRANSMISSION_USERNAME") {
            self.transmission.username = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_TRANSMISSION_PASSWORD") {
            self.transmission.password = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_DELUGE_URL") {
            self.deluge.url = v;
        }
        if let Ok(v) = std::env::var("TNG_DELUGE_PASSWORD") {
            self.deluge.password = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_TORRENTNG_URL") {
            self.torrentng.url = v;
        }
        if let Ok(v) = std::env::var("TNG_TORRENTNG_API_TOKEN") {
            self.torrentng.api_token = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_RTORRENT_LOGS_ENABLED") {
            self.rtorrent.logs.enabled = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("TNG_RTORRENT_LOG_PATHS") {
            self.rtorrent.logs.paths = v
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        if let Ok(v) = std::env::var("TNG_RTORRENT_LOG_POLL_INTERVAL_SECS") {
            if let Ok(secs) = v.parse() {
                self.rtorrent.logs.poll_interval_secs = secs;
            }
        }
        if let Ok(v) = std::env::var("TNG_RTORRENT_LOG_READ_FROM_START") {
            self.rtorrent.logs.read_from_start = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("TNG_SCGI_SOCKET") {
            self.rtorrent.scgi_socket = Some(v);
            self.rtorrent.scgi_addr = None;
        }
        if let Ok(v) = std::env::var("TNG_SCGI_ADDR") {
            self.rtorrent.scgi_addr = Some(v);
            self.rtorrent.scgi_socket = None;
        }
        if let Ok(v) = std::env::var("TNG_SECRET_KEY") {
            self.auth.secret_key = Some(v);
        }
        if let Ok(v) = std::env::var("TNG_API_TOKENS") {
            self.auth.api_tokens = v
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }
        if let Ok(v) = std::env::var("TNG_ALLOW_SCRIPTS") {
            self.workflows.allow_scripts = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_VERSION") {
            self.identity.qbittorrent_version = v;
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_WEBAPI_VERSION") {
            self.identity.qbittorrent_webapi_version = v;
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_BUILD_LIBTORRENT") {
            self.identity.qbittorrent_build_libtorrent = v;
        }
        if let Ok(v) = std::env::var("TNG_QBITTORRENT_BUILD_QT") {
            self.identity.qbittorrent_build_qt = v;
        }
    }

    fn validate(&self) -> Result<()> {
        match self.backend.backend_type {
            BackendKind::Rtorrent => match (&self.rtorrent.scgi_socket, &self.rtorrent.scgi_addr) {
                (None, None) => bail!("rtorrent: one of scgi_socket or scgi_addr must be set"),
                (Some(_), Some(_)) => {
                    bail!("rtorrent: only one of scgi_socket or scgi_addr may be set")
                }
                _ => {
                    if self.rtorrent.peer_id.len() != 20 || !self.rtorrent.peer_id.is_ascii() {
                        bail!("rtorrent: peer_id must be exactly 20 ASCII bytes");
                    }
                }
            },
            BackendKind::Qbittorrent => {
                if self.qbittorrent.url.trim().is_empty() {
                    bail!("qbittorrent: url must be set");
                }
            }
            BackendKind::Transmission => {
                if self.transmission.url.trim().is_empty() {
                    bail!("transmission: url must be set");
                }
            }
            BackendKind::Deluge => {
                if self.deluge.url.trim().is_empty() {
                    bail!("deluge: url must be set");
                }
            }
            BackendKind::Torrentng => {
                if self.torrentng.url.trim().is_empty() {
                    bail!("torrentng: url must be set");
                }
            }
        }
        Ok(())
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend.backend_type {
            BackendKind::Rtorrent => "rtorrent",
            BackendKind::Qbittorrent => "qbittorrent",
            BackendKind::Transmission => "transmission",
            BackendKind::Deluge => "deluge",
            BackendKind::Torrentng => "torrentng",
        }
    }

    pub fn cache_path(&self) -> PathBuf {
        let dir = self.data_dir.clone().unwrap_or_else(|| {
            dirs_next::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("/var/lib"))
                .join("torrentng")
        });
        dir.join("cache.db")
    }

    pub fn sync_interval(&self) -> Duration {
        Duration::from_secs(self.sync_interval_secs)
    }

    pub fn rtorrent_timeout(&self) -> Duration {
        Duration::from_secs(self.rtorrent.timeout_secs)
    }

    pub fn rtorrent_log_poll_interval(&self) -> Duration {
        Duration::from_secs(self.rtorrent.logs.poll_interval_secs.max(1))
    }
}

impl Default for RtorrentLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            paths: Vec::new(),
            poll_interval_secs: 2,
            read_from_start: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn auth_env_overrides_file_values() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_secret = std::env::var("TNG_SECRET_KEY").ok();
        let old_tokens = std::env::var("TNG_API_TOKENS").ok();

        std::env::set_var("TNG_SECRET_KEY", "runtime-secret");
        std::env::set_var("TNG_API_TOKENS", "alpha, beta ,,gamma");

        let mut cfg = Config::test_default();
        cfg.auth.secret_key = Some("file-secret".to_owned());
        cfg.auth.api_tokens = vec!["file-token".to_owned()];
        cfg.apply_env();

        assert_eq!(cfg.auth.secret_key.as_deref(), Some("runtime-secret"));
        assert_eq!(cfg.auth.api_tokens, ["alpha", "beta", "gamma"]);

        restore_env("TNG_SECRET_KEY", old_secret);
        restore_env("TNG_API_TOKENS", old_tokens);
    }

    #[test]
    fn identity_env_overrides_defaults() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_qb = std::env::var("TNG_QBITTORRENT_VERSION").ok();
        let old_api = std::env::var("TNG_QBITTORRENT_WEBAPI_VERSION").ok();
        let old_lt = std::env::var("TNG_QBITTORRENT_BUILD_LIBTORRENT").ok();
        let old_qt = std::env::var("TNG_QBITTORRENT_BUILD_QT").ok();

        std::env::set_var("TNG_QBITTORRENT_VERSION", "4.6.7");
        std::env::set_var("TNG_QBITTORRENT_WEBAPI_VERSION", "2.10.4");
        std::env::set_var("TNG_QBITTORRENT_BUILD_LIBTORRENT", "2.0.10.0");
        std::env::set_var("TNG_QBITTORRENT_BUILD_QT", "6.6.3");

        let mut cfg = Config::test_default();
        cfg.apply_env();

        assert_eq!(cfg.identity.qbittorrent_version, "4.6.7");
        assert_eq!(cfg.identity.qbittorrent_webapi_version, "2.10.4");
        assert_eq!(cfg.identity.qbittorrent_build_libtorrent, "2.0.10.0");
        assert_eq!(cfg.identity.qbittorrent_build_qt, "6.6.3");

        restore_env("TNG_QBITTORRENT_VERSION", old_qb);
        restore_env("TNG_QBITTORRENT_WEBAPI_VERSION", old_api);
        restore_env("TNG_QBITTORRENT_BUILD_LIBTORRENT", old_lt);
        restore_env("TNG_QBITTORRENT_BUILD_QT", old_qt);
    }

    #[test]
    fn backend_env_selects_all_supported_backends() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_backend = std::env::var("TNG_BACKEND").ok();
        for (raw, expected) in [
            ("rtorrent", BackendKind::Rtorrent),
            ("qbittorrent", BackendKind::Qbittorrent),
            ("qbit", BackendKind::Qbittorrent),
            ("transmission", BackendKind::Transmission),
            ("deluge", BackendKind::Deluge),
            ("torrentng", BackendKind::Torrentng),
            ("native", BackendKind::Torrentng),
        ] {
            std::env::set_var("TNG_BACKEND", raw);
            let mut cfg = Config::test_default();
            cfg.apply_env();
            assert_eq!(cfg.backend.backend_type, expected, "{raw}");
        }
        restore_env("TNG_BACKEND", old_backend);
    }

    #[test]
    fn external_backend_env_overrides_connection_fields() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_qb_url = std::env::var("TNG_QBITTORRENT_URL").ok();
        let old_qb_user = std::env::var("TNG_QBITTORRENT_USERNAME").ok();
        let old_qb_pass = std::env::var("TNG_QBITTORRENT_PASSWORD").ok();
        let old_tr_url = std::env::var("TNG_TRANSMISSION_URL").ok();
        let old_tr_user = std::env::var("TNG_TRANSMISSION_USERNAME").ok();
        let old_tr_pass = std::env::var("TNG_TRANSMISSION_PASSWORD").ok();
        let old_de_url = std::env::var("TNG_DELUGE_URL").ok();
        let old_de_pass = std::env::var("TNG_DELUGE_PASSWORD").ok();
        let old_tng_url = std::env::var("TNG_TORRENTNG_URL").ok();
        let old_tng_token = std::env::var("TNG_TORRENTNG_API_TOKEN").ok();

        std::env::set_var("TNG_QBITTORRENT_URL", "http://qbit:8080");
        std::env::set_var("TNG_QBITTORRENT_USERNAME", "qb-user");
        std::env::set_var("TNG_QBITTORRENT_PASSWORD", "qb-pass");
        std::env::set_var("TNG_TRANSMISSION_URL", "http://tr:9091/transmission/rpc");
        std::env::set_var("TNG_TRANSMISSION_USERNAME", "tr-user");
        std::env::set_var("TNG_TRANSMISSION_PASSWORD", "tr-pass");
        std::env::set_var("TNG_DELUGE_URL", "http://deluge:8112/json");
        std::env::set_var("TNG_DELUGE_PASSWORD", "deluge-pass");
        std::env::set_var("TNG_TORRENTNG_URL", "http://native:8080");
        std::env::set_var("TNG_TORRENTNG_API_TOKEN", "native-token");

        let mut cfg = Config::test_default();
        cfg.apply_env();

        assert_eq!(cfg.qbittorrent.url, "http://qbit:8080");
        assert_eq!(cfg.qbittorrent.username.as_deref(), Some("qb-user"));
        assert_eq!(cfg.qbittorrent.password.as_deref(), Some("qb-pass"));
        assert_eq!(cfg.transmission.url, "http://tr:9091/transmission/rpc");
        assert_eq!(cfg.transmission.username.as_deref(), Some("tr-user"));
        assert_eq!(cfg.transmission.password.as_deref(), Some("tr-pass"));
        assert_eq!(cfg.deluge.url, "http://deluge:8112/json");
        assert_eq!(cfg.deluge.password.as_deref(), Some("deluge-pass"));
        assert_eq!(cfg.torrentng.url, "http://native:8080");
        assert_eq!(cfg.torrentng.api_token.as_deref(), Some("native-token"));

        restore_env("TNG_QBITTORRENT_URL", old_qb_url);
        restore_env("TNG_QBITTORRENT_USERNAME", old_qb_user);
        restore_env("TNG_QBITTORRENT_PASSWORD", old_qb_pass);
        restore_env("TNG_TRANSMISSION_URL", old_tr_url);
        restore_env("TNG_TRANSMISSION_USERNAME", old_tr_user);
        restore_env("TNG_TRANSMISSION_PASSWORD", old_tr_pass);
        restore_env("TNG_DELUGE_URL", old_de_url);
        restore_env("TNG_DELUGE_PASSWORD", old_de_pass);
        restore_env("TNG_TORRENTNG_URL", old_tng_url);
        restore_env("TNG_TORRENTNG_API_TOKEN", old_tng_token);
    }

    #[test]
    fn rtorrent_log_env_overrides_defaults() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_enabled = std::env::var("TNG_RTORRENT_LOGS_ENABLED").ok();
        let old_paths = std::env::var("TNG_RTORRENT_LOG_PATHS").ok();
        let old_poll = std::env::var("TNG_RTORRENT_LOG_POLL_INTERVAL_SECS").ok();
        let old_start = std::env::var("TNG_RTORRENT_LOG_READ_FROM_START").ok();

        std::env::set_var("TNG_RTORRENT_LOGS_ENABLED", "true");
        std::env::set_var(
            "TNG_RTORRENT_LOG_PATHS",
            "/var/log/rtorrent.log, /tmp/rtorrent.log",
        );
        std::env::set_var("TNG_RTORRENT_LOG_POLL_INTERVAL_SECS", "9");
        std::env::set_var("TNG_RTORRENT_LOG_READ_FROM_START", "1");

        let mut cfg = Config::test_default();
        cfg.apply_env();

        assert!(cfg.rtorrent.logs.enabled);
        assert_eq!(
            cfg.rtorrent.logs.paths,
            [
                PathBuf::from("/var/log/rtorrent.log"),
                PathBuf::from("/tmp/rtorrent.log")
            ]
        );
        assert_eq!(cfg.rtorrent.logs.poll_interval_secs, 9);
        assert!(cfg.rtorrent.logs.read_from_start);

        restore_env("TNG_RTORRENT_LOGS_ENABLED", old_enabled);
        restore_env("TNG_RTORRENT_LOG_PATHS", old_paths);
        restore_env("TNG_RTORRENT_LOG_POLL_INTERVAL_SECS", old_poll);
        restore_env("TNG_RTORRENT_LOG_READ_FROM_START", old_start);
    }

    #[test]
    fn rtorrent_log_poll_interval_has_floor() {
        let mut cfg = Config::test_default();
        cfg.rtorrent.logs.poll_interval_secs = 0;
        assert_eq!(cfg.rtorrent_log_poll_interval(), Duration::from_secs(1));
    }

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn sync_and_data_dir_env_override_file_values() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_sync = std::env::var("TNG_SYNC_INTERVAL_SECS").ok();
        let old_data_dir = std::env::var("TNG_DATA_DIR").ok();
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
listen_addr = "127.0.0.1:8080"
sync_interval_secs = 2
data_dir = "/tmp/file-data"

[rtorrent]
scgi_socket = "/tmp/rtorrent.sock"
"#,
        )
        .unwrap();

        std::env::set_var("TNG_SYNC_INTERVAL_SECS", "86400");
        std::env::set_var("TNG_DATA_DIR", "/tmp/env-data");

        let cfg = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        assert_eq!(cfg.sync_interval_secs, 86400);
        assert_eq!(cfg.data_dir, Some(PathBuf::from("/tmp/env-data")));

        restore_env("TNG_SYNC_INTERVAL_SECS", old_sync);
        restore_env("TNG_DATA_DIR", old_data_dir);
    }
}
