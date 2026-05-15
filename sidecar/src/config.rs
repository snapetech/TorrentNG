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

    pub rtorrent: RtorrentConfig,

    #[serde(default)]
    pub auth: AuthConfig,
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

impl Config {
    /// Minimal config for unit/integration tests — no real rTorrent connection.
    pub fn test_default() -> Self {
        Self {
            listen_addr: "127.0.0.1:0".into(),
            debug: false,
            sync_interval_secs: 60,
            data_dir: None,
            rtorrent: RtorrentConfig {
                scgi_socket: Some("/nonexistent".into()),
                scgi_addr: None,
                timeout_secs: 1,
                user_agent: DEFAULT_USER_AGENT.to_owned(),
            },
            auth: AuthConfig::default(),
        }
    }
}

// --- defaults ---

fn default_listen_addr() -> String { "0.0.0.0:8080".into() }
fn default_sync_interval_secs() -> u64 { 2 }
fn default_timeout_secs() -> u64 { 10 }
fn default_user_agent() -> String { DEFAULT_USER_AGENT.to_owned() }

// --- loading ---

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        let path = match path {
            Some(p) => PathBuf::from(p),
            None => {
                let home = dirs_next::home_dir()
                    .context("cannot determine home directory")?;
                home.join(".config").join("rtorrentng").join("config.toml")
            }
        };

        let mut cfg: Config = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read config {}", path.display()))?;
            toml::from_str(&raw)
                .with_context(|| format!("parse config {}", path.display()))?
        } else {
            // No config file — synthesize a minimal default.
            // rtorrent.scgi_socket is required; will be caught in validate().
            toml::from_str(r#"
                [rtorrent]
                scgi_socket = "/run/rtorrent/rpc.sock"
            "#)?
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
    }

    fn validate(&self) -> Result<()> {
        match (&self.rtorrent.scgi_socket, &self.rtorrent.scgi_addr) {
            (None, None) => bail!("rtorrent: one of scgi_socket or scgi_addr must be set"),
            (Some(_), Some(_)) => bail!("rtorrent: only one of scgi_socket or scgi_addr may be set"),
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
