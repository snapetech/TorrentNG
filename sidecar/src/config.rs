use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

/// Default user-agent announced to trackers via rTorrent.
/// Overridden by config or RTNG_USER_AGENT env var.
pub const DEFAULT_USER_AGENT: &str = "rtorrentNG/0.1.0 libtorrent/0.16.11";

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

    pub rtorrent: RtorrentConfig,

    #[serde(default)]
    pub auth: AuthConfig,

    #[serde(default)]
    pub workflows: WorkflowConfig,

    #[serde(default)]
    pub identity: IdentityConfig,
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
    /// Override with RTNG_USER_AGENT env var.
    /// See docs/CONFIGURATION.md for accepted values.
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
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

impl Config {
    /// Minimal config for unit/integration tests — no real rTorrent connection.
    pub fn test_default() -> Self {
        Self {
            listen_addr: "127.0.0.1:0".into(),
            debug: false,
            sync_interval_secs: 60,
            data_dir: None,
            storage_roots: vec![PathBuf::from("/")],
            rtorrent: RtorrentConfig {
                scgi_socket: Some("/nonexistent".into()),
                scgi_addr: None,
                timeout_secs: 1,
                user_agent: DEFAULT_USER_AGENT.to_owned(),
            },
            auth: AuthConfig::default(),
            workflows: WorkflowConfig::default(),
            identity: IdentityConfig::default(),
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

// --- loading ---

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let home = dirs_next::home_dir().context("cannot determine home directory")?;
                home.join(".config").join("rtorrentng").join("config.toml")
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
        if let Ok(v) = std::env::var("RTNG_LISTEN_ADDR") {
            self.listen_addr = v;
        }
        if let Ok(v) = std::env::var("RTNG_DEBUG") {
            self.debug = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("RTNG_SYNC_INTERVAL_SECS") {
            if let Ok(secs) = v.parse() {
                self.sync_interval_secs = secs;
            }
        }
        if let Ok(v) = std::env::var("RTNG_DATA_DIR") {
            self.data_dir = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("RTNG_USER_AGENT") {
            self.rtorrent.user_agent = v;
        }
        if let Ok(v) = std::env::var("RTNG_SCGI_SOCKET") {
            self.rtorrent.scgi_socket = Some(v);
            self.rtorrent.scgi_addr = None;
        }
        if let Ok(v) = std::env::var("RTNG_SCGI_ADDR") {
            self.rtorrent.scgi_addr = Some(v);
            self.rtorrent.scgi_socket = None;
        }
        if let Ok(v) = std::env::var("RTNG_SECRET_KEY") {
            self.auth.secret_key = Some(v);
        }
        if let Ok(v) = std::env::var("RTNG_API_TOKENS") {
            self.auth.api_tokens = v
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }
        if let Ok(v) = std::env::var("RTNG_ALLOW_SCRIPTS") {
            self.workflows.allow_scripts = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("RTNG_QBITTORRENT_VERSION") {
            self.identity.qbittorrent_version = v;
        }
        if let Ok(v) = std::env::var("RTNG_QBITTORRENT_WEBAPI_VERSION") {
            self.identity.qbittorrent_webapi_version = v;
        }
        if let Ok(v) = std::env::var("RTNG_QBITTORRENT_BUILD_LIBTORRENT") {
            self.identity.qbittorrent_build_libtorrent = v;
        }
        if let Ok(v) = std::env::var("RTNG_QBITTORRENT_BUILD_QT") {
            self.identity.qbittorrent_build_qt = v;
        }
    }

    fn validate(&self) -> Result<()> {
        match (&self.rtorrent.scgi_socket, &self.rtorrent.scgi_addr) {
            (None, None) => bail!("rtorrent: one of scgi_socket or scgi_addr must be set"),
            (Some(_), Some(_)) => {
                bail!("rtorrent: only one of scgi_socket or scgi_addr may be set")
            }
            _ => {}
        }
        Ok(())
    }

    pub fn cache_path(&self) -> PathBuf {
        let dir = self.data_dir.clone().unwrap_or_else(|| {
            dirs_next::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("/var/lib"))
                .join("rtorrentng")
        });
        dir.join("cache.db")
    }

    pub fn sync_interval(&self) -> Duration {
        Duration::from_secs(self.sync_interval_secs)
    }

    pub fn rtorrent_timeout(&self) -> Duration {
        Duration::from_secs(self.rtorrent.timeout_secs)
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
        let old_secret = std::env::var("RTNG_SECRET_KEY").ok();
        let old_tokens = std::env::var("RTNG_API_TOKENS").ok();

        std::env::set_var("RTNG_SECRET_KEY", "runtime-secret");
        std::env::set_var("RTNG_API_TOKENS", "alpha, beta ,,gamma");

        let mut cfg = Config::test_default();
        cfg.auth.secret_key = Some("file-secret".to_owned());
        cfg.auth.api_tokens = vec!["file-token".to_owned()];
        cfg.apply_env();

        assert_eq!(cfg.auth.secret_key.as_deref(), Some("runtime-secret"));
        assert_eq!(cfg.auth.api_tokens, ["alpha", "beta", "gamma"]);

        restore_env("RTNG_SECRET_KEY", old_secret);
        restore_env("RTNG_API_TOKENS", old_tokens);
    }

    #[test]
    fn identity_env_overrides_defaults() {
        let _guard = ENV_LOCK.lock().expect("env lock poisoned");
        let old_qb = std::env::var("RTNG_QBITTORRENT_VERSION").ok();
        let old_api = std::env::var("RTNG_QBITTORRENT_WEBAPI_VERSION").ok();
        let old_lt = std::env::var("RTNG_QBITTORRENT_BUILD_LIBTORRENT").ok();
        let old_qt = std::env::var("RTNG_QBITTORRENT_BUILD_QT").ok();

        std::env::set_var("RTNG_QBITTORRENT_VERSION", "4.6.7");
        std::env::set_var("RTNG_QBITTORRENT_WEBAPI_VERSION", "2.10.4");
        std::env::set_var("RTNG_QBITTORRENT_BUILD_LIBTORRENT", "2.0.10.0");
        std::env::set_var("RTNG_QBITTORRENT_BUILD_QT", "6.6.3");

        let mut cfg = Config::test_default();
        cfg.apply_env();

        assert_eq!(cfg.identity.qbittorrent_version, "4.6.7");
        assert_eq!(cfg.identity.qbittorrent_webapi_version, "2.10.4");
        assert_eq!(cfg.identity.qbittorrent_build_libtorrent, "2.0.10.0");
        assert_eq!(cfg.identity.qbittorrent_build_qt, "6.6.3");

        restore_env("RTNG_QBITTORRENT_VERSION", old_qb);
        restore_env("RTNG_QBITTORRENT_WEBAPI_VERSION", old_api);
        restore_env("RTNG_QBITTORRENT_BUILD_LIBTORRENT", old_lt);
        restore_env("RTNG_QBITTORRENT_BUILD_QT", old_qt);
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
        let old_sync = std::env::var("RTNG_SYNC_INTERVAL_SECS").ok();
        let old_data_dir = std::env::var("RTNG_DATA_DIR").ok();
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

        std::env::set_var("RTNG_SYNC_INTERVAL_SECS", "86400");
        std::env::set_var("RTNG_DATA_DIR", "/tmp/env-data");

        let cfg = Config::load(Some(config_path.to_str().unwrap())).unwrap();
        assert_eq!(cfg.sync_interval_secs, 86400);
        assert_eq!(cfg.data_dir, Some(PathBuf::from("/tmp/env-data")));

        restore_env("RTNG_SYNC_INTERVAL_SECS", old_sync);
        restore_env("RTNG_DATA_DIR", old_data_dir);
    }
}
