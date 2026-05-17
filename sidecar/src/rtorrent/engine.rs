use serde::Serialize;

use super::client::{Client, XmlValue};

#[derive(Debug, Clone, Serialize)]
pub struct EngineDiagnostics {
    pub provenance: EngineProvenance,
    pub capabilities: Vec<EngineCapability>,
    pub http: HttpStackDiagnostics,
    pub dht: DhtDiagnostics,
    pub drift: Vec<EngineDrift>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineCommandIndex {
    pub ok: bool,
    pub count: usize,
    pub commands: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineProvenance {
    pub sidecar_version: &'static str,
    pub rtorrent_version: Option<String>,
    pub libtorrent_version: Option<String>,
    pub xmlrpc_backend: &'static str,
    pub packaged_rtorrent_version: Option<String>,
    pub packaged_libtorrent_version: Option<String>,
    pub patch_set: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineCapability {
    pub key: &'static str,
    pub label: &'static str,
    pub command: &'static str,
    pub available: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HttpStackDiagnostics {
    pub user_agent: ProbeValue<String>,
    pub current_open: ProbeValue<i64>,
    pub max_total_connections: ProbeValue<i64>,
    pub max_host_connections: ProbeValue<i64>,
    pub max_cache_connections: ProbeValue<i64>,
    pub dns_cache_timeout: ProbeValue<i64>,
    pub proxy_address: ProbeValue<String>,
    pub ca_path: ProbeValue<String>,
    pub ca_cert: ProbeValue<String>,
    pub ssl_verify_peer: ProbeValue<bool>,
    pub ssl_verify_host: ProbeValue<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DhtDiagnostics {
    pub enabled: ProbeValue<String>,
    pub port: ProbeValue<i64>,
    pub override_port: ProbeValue<i64>,
    pub listen_port: ProbeValue<i64>,
    pub listen_range: ProbeValue<String>,
    pub pex: ProbeValue<bool>,
    pub udp_trackers: ProbeValue<bool>,
    pub statistics: ProbeValue<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineDrift {
    pub key: &'static str,
    pub label: &'static str,
    pub command: &'static str,
    pub expected: String,
    pub actual: Option<String>,
    pub status: DriftStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftStatus {
    Match,
    Mismatch,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeValue<T: Serialize> {
    pub ok: bool,
    pub value: Option<T>,
    pub error: Option<String>,
}

impl<T: Serialize> ProbeValue<T> {
    pub fn ok(value: T) -> Self {
        Self {
            ok: true,
            value: Some(value),
            error: None,
        }
    }

    pub fn err(error: String) -> Self {
        Self {
            ok: false,
            value: None,
            error: Some(error),
        }
    }
}

impl Client {
    pub async fn engine_diagnostics(&self) -> EngineDiagnostics {
        let methods = self.list_methods().await.unwrap_or_default();
        EngineDiagnostics {
            provenance: EngineProvenance {
                sidecar_version: env!("CARGO_PKG_VERSION"),
                rtorrent_version: self.optional_string("system.client_version").await,
                libtorrent_version: self.optional_string("system.library_version").await,
                xmlrpc_backend: "tinyxml2",
                packaged_rtorrent_version: std::env::var("TNG_PACKAGED_RTORRENT_VERSION").ok(),
                packaged_libtorrent_version: std::env::var("TNG_PACKAGED_LIBTORRENT_VERSION").ok(),
                patch_set: std::env::var("TNG_RTORRENT_PATCHES")
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            capabilities: capability_matrix(&methods),
            http: self.http_stack_diagnostics().await,
            dht: self.dht_diagnostics().await,
            drift: self.engine_profile_drift().await,
        }
    }

    pub async fn command_index(&self) -> EngineCommandIndex {
        match self.list_methods().await {
            Ok(mut commands) => {
                commands.sort();
                EngineCommandIndex {
                    ok: true,
                    count: commands.len(),
                    commands,
                    error: None,
                }
            }
            Err(e) => EngineCommandIndex {
                ok: false,
                count: 0,
                commands: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    async fn http_stack_diagnostics(&self) -> HttpStackDiagnostics {
        HttpStackDiagnostics {
            user_agent: self.probe_string("network.http.user_agent").await,
            current_open: self.probe_i64("network.http.current_open").await,
            max_total_connections: self.probe_i64("network.http.max_total_connections").await,
            max_host_connections: self.probe_i64("network.http.max_host_connections").await,
            max_cache_connections: self.probe_i64("network.http.max_cache_connections").await,
            dns_cache_timeout: self.probe_i64("network.http.dns_cache_timeout").await,
            proxy_address: self.probe_string("network.http.proxy_address").await,
            ca_path: self.probe_string("network.http.capath").await,
            ca_cert: self.probe_string("network.http.cacert").await,
            ssl_verify_peer: self.probe_bool("network.http.ssl_verify_peer").await,
            ssl_verify_host: self.probe_bool("network.http.ssl_verify_host").await,
        }
    }

    async fn dht_diagnostics(&self) -> DhtDiagnostics {
        DhtDiagnostics {
            enabled: self.probe_string("dht").await,
            port: self.probe_i64("dht.port").await,
            override_port: self.probe_i64("dht.override_port").await,
            listen_port: self.probe_i64("network.listen.port").await,
            listen_range: self.probe_string("network.port_range").await,
            pex: self.probe_bool("protocol.pex").await,
            udp_trackers: self.probe_bool("trackers.use_udp").await,
            statistics: self.probe_display("dht.statistics").await,
        }
    }

    async fn engine_profile_drift(&self) -> Vec<EngineDrift> {
        let incoming_port =
            std::env::var("RTORRENT_INCOMING_PORT").unwrap_or_else(|_| "50000".to_owned());
        let listen_range = format!("{incoming_port}-{incoming_port}");
        const EXPECTED: &[(&str, &str, &str, &str)] = &[
            (
                "port_random",
                "Random listen port",
                "network.port_random",
                "false",
            ),
            ("pex", "Peer exchange", "protocol.pex", "true"),
            ("udp_trackers", "UDP trackers", "trackers.use_udp", "true"),
            (
                "max_uploads_global",
                "Global upload slots",
                "throttle.max_uploads.global",
                "500",
            ),
            (
                "max_downloads_global",
                "Global download slots",
                "throttle.max_downloads.global",
                "50",
            ),
            (
                "max_uploads",
                "Per-torrent upload slots",
                "throttle.max_uploads",
                "10",
            ),
            (
                "max_downloads",
                "Per-torrent download slots",
                "throttle.max_downloads",
                "4",
            ),
            (
                "hash_on_completion",
                "Hash on completion",
                "pieces.hash.on_completion",
                "true",
            ),
            ("session_lock", "Session lock", "session.use_lock", "true"),
            (
                "session_on_completion",
                "Persist completion state",
                "session.on_completion",
                "true",
            ),
            (
                "tracker_numwant",
                "Tracker numwant",
                "trackers.numwant",
                "80",
            ),
        ];

        let mut rows = Vec::with_capacity(EXPECTED.len() + 1);
        rows.push(
            self.drift_probe_owned(
                "port_range",
                "Listen port range",
                "network.port_range",
                listen_range,
            )
            .await,
        );
        for (key, label, command, expected) in EXPECTED {
            rows.push(self.drift_probe(key, label, command, expected).await);
        }
        rows
    }

    async fn drift_probe(
        &self,
        key: &'static str,
        label: &'static str,
        command: &'static str,
        expected: &'static str,
    ) -> EngineDrift {
        self.drift_probe_owned(key, label, command, expected.to_owned())
            .await
    }

    async fn drift_probe_owned(
        &self,
        key: &'static str,
        label: &'static str,
        command: &'static str,
        expected: String,
    ) -> EngineDrift {
        match self.call(command, &[]).await {
            Ok(v) => {
                let actual = xml_value_display(&v);
                let status = if drift_matches(&actual, &expected) {
                    DriftStatus::Match
                } else {
                    DriftStatus::Mismatch
                };
                EngineDrift {
                    key,
                    label,
                    command,
                    expected,
                    actual: Some(actual),
                    status,
                    detail: None,
                }
            }
            Err(e) => EngineDrift {
                key,
                label,
                command,
                expected,
                actual: None,
                status: DriftStatus::Unavailable,
                detail: Some(e.to_string()),
            },
        }
    }

    async fn optional_string(&self, method: &str) -> Option<String> {
        self.call(method, &[])
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
    }

    async fn probe_string(&self, method: &str) -> ProbeValue<String> {
        match self.call(method, &[]).await {
            Ok(v) => ProbeValue::ok(v.as_str().unwrap_or("").to_owned()),
            Err(e) => ProbeValue::err(e.to_string()),
        }
    }

    async fn probe_i64(&self, method: &str) -> ProbeValue<i64> {
        match self.call(method, &[]).await {
            Ok(v) => ProbeValue::ok(v.as_i64().unwrap_or(0)),
            Err(e) => ProbeValue::err(e.to_string()),
        }
    }

    async fn probe_bool(&self, method: &str) -> ProbeValue<bool> {
        match self.call(method, &[]).await {
            Ok(v) => ProbeValue::ok(v.as_bool().unwrap_or(false)),
            Err(e) => ProbeValue::err(e.to_string()),
        }
    }

    async fn probe_display(&self, method: &str) -> ProbeValue<String> {
        match self.call(method, &[]).await {
            Ok(v) => ProbeValue::ok(xml_value_display(&v)),
            Err(e) => ProbeValue::err(e.to_string()),
        }
    }
}

fn xml_value_display(value: &XmlValue) -> String {
    match value {
        XmlValue::String(s) => s.trim().to_owned(),
        XmlValue::Base64(s) => s.trim().to_owned(),
        XmlValue::Int(n) => n.to_string(),
        XmlValue::Bool(b) => b.to_string(),
        XmlValue::Array(items) => items
            .iter()
            .map(xml_value_display)
            .collect::<Vec<_>>()
            .join(","),
        XmlValue::Struct(_) => "struct".to_owned(),
        XmlValue::Nil => String::new(),
    }
}

fn drift_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
        || (expected == "true" && actual == "1")
        || (expected == "false" && actual == "0")
}

fn capability_matrix(methods: &[String]) -> Vec<EngineCapability> {
    const CAPS: &[(&str, &str, &str)] = &[
        ("method_index", "XMLRPC method index", "method.list_keys"),
        (
            "tracker_user_agent",
            "Tracker HTTP user-agent control",
            "network.http.user_agent.set",
        ),
        (
            "http_stack_metrics",
            "HTTP tracker stack telemetry",
            "network.http.current_open",
        ),
        (
            "http_proxy",
            "HTTP tracker proxy",
            "network.http.proxy_address.set",
        ),
        (
            "ssl_verify",
            "Tracker TLS verification controls",
            "network.http.ssl_verify_peer.set",
        ),
        (
            "trusted_rpc_toggle",
            "RPC trusted connection toggle",
            "rpc.trusted_connection_accept_all.set",
        ),
        ("jsonrpc", "JSON-RPC transport", "network.rpc.use_jsonrpc"),
        ("raw_torrent_load", "Raw torrent load", "load.raw_start"),
        ("magnet_load", "Magnet load", "load.start"),
        (
            "tracker_announce",
            "Manual tracker announce",
            "d.tracker_announce",
        ),
        ("tracker_insert", "Tracker insert", "d.tracker.insert"),
        (
            "scgi_gzip",
            "SCGI gzip controls",
            "network.scgi.use_gzip.set",
        ),
        (
            "bounded_multicall",
            "Bounded torrent multicall",
            "d.multicall.range",
        ),
        (
            "live_rate_multicall",
            "Live-rate torrent multicall",
            "d.multicall.nonzero_rate",
        ),
        (
            "live_summary",
            "TorrentNG live summary",
            "tng.live_summary",
        ),
    ];

    CAPS.iter()
        .map(|(key, label, command)| {
            let available = methods.iter().any(|m| m == command);
            EngineCapability {
                key,
                label,
                command,
                available,
                detail: if available {
                    None
                } else {
                    Some("command not exposed by running rTorrent build".to_owned())
                },
            }
        })
        .collect()
}
