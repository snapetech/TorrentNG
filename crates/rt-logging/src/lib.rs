use serde::{Deserialize, Serialize};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LoggingConfig {
    pub format: LogFormat,
    pub profile: LogProfile,
    pub filter: String,
    pub event_retention: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogProfile {
    Basic,
    Detailed,
    Verbose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLogging {
    pub format: LogFormat,
    pub profile: LogProfile,
    pub filter: String,
    pub source: &'static str,
    pub event_retention: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: LogFormat::Json,
            profile: LogProfile::Basic,
            filter: String::new(),
            event_retention: 10_000,
        }
    }
}

pub fn profile_filter(profile: LogProfile) -> &'static str {
    match profile {
        LogProfile::Basic => "info,tower_http=warn,hyper=warn",
        LogProfile::Detailed => "info,rt_engine=debug,rt_api_qbit=debug,rt_api_native=debug,torrentng=debug,tower_http=info,hyper=warn",
        LogProfile::Verbose => "debug,rt_engine=trace,rt_storage=trace,rt_tracker=trace,rt_peer_wire=trace,rt_api_qbit=trace,rt_api_native=trace,torrentng=trace,tower_http=debug,hyper=info",
    }
}

pub fn effective(config: &LoggingConfig, legacy_filter: Option<&str>) -> EffectiveLogging {
    if let Ok(filter) = std::env::var("RUST_LOG") {
        return build(config, filter, "RUST_LOG");
    }
    if !config.filter.trim().is_empty() {
        return build(config, config.filter.clone(), "logging.filter");
    }
    if config.profile != LogProfile::Basic {
        return build(
            config,
            profile_filter(config.profile).to_owned(),
            "logging.profile",
        );
    }
    if let Some(filter) = legacy_filter.filter(|s| !s.trim().is_empty()) {
        return build(config, filter.to_owned(), "legacy");
    }
    build(
        config,
        profile_filter(config.profile).to_owned(),
        "logging.profile",
    )
}

fn build(config: &LoggingConfig, filter: String, source: &'static str) -> EffectiveLogging {
    EffectiveLogging {
        format: config.format,
        profile: config.profile,
        filter,
        source,
        event_retention: config.event_retention.max(1),
    }
}

pub fn init(config: &LoggingConfig, legacy_filter: Option<&str>) -> EffectiveLogging {
    let effective = effective(config, legacy_filter);
    let filter = EnvFilter::try_new(&effective.filter).unwrap_or_else(|_| EnvFilter::new("info"));
    match effective.format {
        LogFormat::Json => fmt().with_env_filter(filter).json().init(),
        LogFormat::Pretty => fmt().with_env_filter(filter).pretty().init(),
    }
    tracing::info!(
        component = "logging",
        operation = "init",
        format = ?effective.format,
        profile = ?effective.profile,
        filter = %effective.filter,
        source = effective.source,
        event_retention = effective.event_retention,
        "logging initialized"
    );
    effective
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn profile_filters_have_expected_escalation() {
        assert!(profile_filter(LogProfile::Basic).starts_with("info"));
        assert!(profile_filter(LogProfile::Detailed).contains("rt_engine=debug"));
        assert!(profile_filter(LogProfile::Verbose).contains("rt_engine=trace"));
    }

    #[test]
    fn precedence_prefers_env_then_filter_then_profile_then_legacy() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var("RUST_LOG").ok();
        std::env::remove_var("RUST_LOG");
        let mut cfg = LoggingConfig::default();
        assert_eq!(effective(&cfg, Some("warn")).source, "legacy");
        cfg.profile = LogProfile::Detailed;
        assert_eq!(effective(&cfg, Some("warn")).source, "logging.profile");
        cfg.filter = "rt_engine=warn".into();
        assert_eq!(effective(&cfg, Some("warn")).source, "logging.filter");
        std::env::set_var("RUST_LOG", "trace");
        assert_eq!(effective(&cfg, Some("warn")).source, "RUST_LOG");
        if let Some(old) = old {
            std::env::set_var("RUST_LOG", old);
        } else {
            std::env::remove_var("RUST_LOG");
        }
    }
}
