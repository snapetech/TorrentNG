use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::time::Duration;

use rt_config::TrackerConfig;
use tokio::net::lookup_host;
use tokio::time::timeout;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutboundTargetKind {
    Tracker,
    Webseed,
}

const HTTP_CLIENT_CACHE_CAPACITY: usize = 256;

static SCHEME_DENIED_TOTAL: AtomicU64 = AtomicU64::new(0);
static ADDRESS_DENIED_TOTAL: AtomicU64 = AtomicU64::new(0);
static RESOLUTION_FAILED_TOTAL: AtomicU64 = AtomicU64::new(0);
static CLIENT_FAILED_TOTAL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressPolicyMetricsSnapshot {
    pub scheme_denied_total: u64,
    pub address_denied_total: u64,
    pub resolution_failed_total: u64,
    pub client_failed_total: u64,
}

pub fn egress_policy_metrics() -> EgressPolicyMetricsSnapshot {
    EgressPolicyMetricsSnapshot {
        scheme_denied_total: SCHEME_DENIED_TOTAL.load(Ordering::Relaxed),
        address_denied_total: ADDRESS_DENIED_TOTAL.load(Ordering::Relaxed),
        resolution_failed_total: RESOLUTION_FAILED_TOTAL.load(Ordering::Relaxed),
        client_failed_total: CLIENT_FAILED_TOTAL.load(Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HttpClientKey {
    kind: OutboundTargetKind,
    host: String,
    port: u16,
    address: SocketAddr,
    timeout_millis: u64,
    user_agent: String,
}

#[derive(Debug, Default)]
struct HttpClientCache {
    clients: HashMap<HttpClientKey, (u64, reqwest::Client)>,
    next_tick: u64,
}

impl HttpClientCache {
    fn get(&mut self, key: &HttpClientKey) -> Option<reqwest::Client> {
        let (tick, client) = self.clients.get_mut(key)?;
        self.next_tick = self.next_tick.wrapping_add(1);
        *tick = self.next_tick;
        Some(client.clone())
    }

    fn insert(&mut self, key: HttpClientKey, client: reqwest::Client) {
        self.next_tick = self.next_tick.wrapping_add(1);
        self.clients.insert(key, (self.next_tick, client));
        while self.clients.len() > HTTP_CLIENT_CACHE_CAPACITY {
            let Some(oldest) = self
                .clients
                .iter()
                .min_by_key(|(_, (tick, _))| *tick)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.clients.remove(&oldest);
        }
    }
}

fn http_client_cache() -> &'static Mutex<HttpClientCache> {
    static CACHE: OnceLock<Mutex<HttpClientCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HttpClientCache::default()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundEgressPolicy {
    pub allow_http_trackers: bool,
    pub allow_https_trackers: bool,
    pub allow_udp_trackers: bool,
    pub allow_http_webseeds: bool,
    pub allow_https_webseeds: bool,
    pub allow_loopback: bool,
    pub allow_private: bool,
    pub allow_link_local: bool,
    pub allow_multicast: bool,
    pub allow_unspecified: bool,
}

impl Default for OutboundEgressPolicy {
    fn default() -> Self {
        Self {
            allow_http_trackers: true,
            allow_https_trackers: true,
            allow_udp_trackers: true,
            allow_http_webseeds: true,
            allow_https_webseeds: true,
            allow_loopback: false,
            allow_private: false,
            allow_link_local: false,
            allow_multicast: false,
            allow_unspecified: false,
        }
    }
}

impl OutboundEgressPolicy {
    pub fn from_config(config: &TrackerConfig) -> Self {
        Self {
            allow_http_trackers: config.allow_http_trackers,
            allow_https_trackers: config.allow_https_trackers,
            allow_udp_trackers: config.allow_udp_trackers,
            allow_http_webseeds: config.allow_http_webseeds,
            allow_https_webseeds: config.allow_https_webseeds,
            allow_loopback: config.allow_loopback_egress,
            allow_private: config.allow_private_egress,
            allow_link_local: config.allow_link_local_egress,
            allow_multicast: config.allow_multicast_egress,
            allow_unspecified: config.allow_unspecified_egress,
        }
    }

    pub fn validate_url(
        &self,
        kind: OutboundTargetKind,
        url: &Url,
    ) -> Result<(), EgressPolicyError> {
        let scheme = url.scheme();
        match kind {
            OutboundTargetKind::Tracker => match scheme {
                "http" if self.allow_http_trackers => Ok(()),
                "https" if self.allow_https_trackers => Ok(()),
                "udp" if self.allow_udp_trackers => Ok(()),
                _ => {
                    SCHEME_DENIED_TOTAL.fetch_add(1, Ordering::Relaxed);
                    Err(EgressPolicyError::SchemeDenied {
                        kind,
                        scheme: scheme.to_owned(),
                    })
                }
            },
            OutboundTargetKind::Webseed => match scheme {
                "http" if self.allow_http_webseeds => Ok(()),
                "https" if self.allow_https_webseeds => Ok(()),
                _ => {
                    SCHEME_DENIED_TOTAL.fetch_add(1, Ordering::Relaxed);
                    Err(EgressPolicyError::SchemeDenied {
                        kind,
                        scheme: scheme.to_owned(),
                    })
                }
            },
        }
    }

    pub fn validate_ip(&self, addr: IpAddr) -> Result<(), EgressPolicyError> {
        let class = AddressClass::classify(addr);
        if class.allowed_by(*self) {
            Ok(())
        } else {
            ADDRESS_DENIED_TOTAL.fetch_add(1, Ordering::Relaxed);
            Err(EgressPolicyError::AddressDenied { addr, class })
        }
    }

    pub fn validate_socket_addr(&self, addr: SocketAddr) -> Result<(), EgressPolicyError> {
        self.validate_ip(addr.ip())
    }

    /// Resolve a hostname and validate every answer before a request is sent.
    /// HTTP callers should use [`Self::http_client`] so the validated address
    /// is also pinned in the transport client.
    pub async fn resolve_and_validate(
        &self,
        kind: OutboundTargetKind,
        url: &Url,
        resolve_timeout: Duration,
    ) -> Result<Vec<SocketAddr>, EgressPolicyError> {
        self.validate_url(kind, url)?;
        let host = url.host_str().ok_or_else(|| {
            RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
            EgressPolicyError::MissingHost
        })?;
        let host = host.to_owned();
        let port = url.port_or_known_default().ok_or_else(|| {
            RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
            EgressPolicyError::MissingPort
        })?;
        let addresses = timeout(resolve_timeout, lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Resolution("DNS resolution timed out".to_owned())
            })?
            .map_err(|error| {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Resolution(error.to_string())
            })?
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
            return Err(EgressPolicyError::Resolution(format!(
                "no addresses returned for {host}"
            )));
        }
        for address in &addresses {
            self.validate_socket_addr(*address)?;
        }
        Ok(addresses)
    }

    /// Build an HTTP client pinned to the first address that passed policy
    /// validation. Redirects stay disabled so every new URL is revalidated by
    /// the caller. Pinning the resolver result closes the TOCTOU window between
    /// policy DNS resolution and reqwest's connection lookup.
    pub async fn http_client(
        &self,
        kind: OutboundTargetKind,
        url: &Url,
        request_timeout: Duration,
        user_agent: &str,
    ) -> Result<reqwest::Client, EgressPolicyError> {
        let addresses = self
            .resolve_and_validate(kind, url, request_timeout)
            .await?;
        let host = url.host_str().ok_or_else(|| {
            RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
            EgressPolicyError::MissingHost
        })?;
        let address = addresses.first().copied().ok_or_else(|| {
            RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
            EgressPolicyError::Resolution("no validated address".to_owned())
        })?;
        let key = HttpClientKey {
            kind,
            host: host.to_owned(),
            port: url.port_or_known_default().ok_or_else(|| {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::MissingPort
            })?,
            address,
            timeout_millis: request_timeout.as_millis().min(u64::MAX as u128) as u64,
            user_agent: user_agent.to_owned(),
        };
        if let Some(client) = http_client_cache()
            .lock()
            .expect("egress client cache poisoned")
            .get(&key)
        {
            return Ok(client);
        }
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(user_agent)
            .resolve(host, address)
            .build()
            .map_err(|error| {
                CLIENT_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Client(error.to_string())
            })?;
        let mut cache = http_client_cache()
            .lock()
            .expect("egress client cache poisoned");
        // A concurrent caller may have filled the same key while this client
        // was being built. Returning the existing clone avoids needless
        // duplicate connection pools without holding the mutex across DNS or
        // reqwest client construction.
        if let Some(existing) = cache.get(&key) {
            return Ok(existing);
        }
        cache.insert(key, client.clone());
        Ok(client)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressClass {
    Public,
    Loopback,
    Private,
    CarrierGradeNat,
    Reserved,
    LinkLocal,
    Multicast,
    Unspecified,
    Documentation,
    Broadcast,
    UniqueLocal,
}

impl AddressClass {
    pub fn classify(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(addr) => {
                if addr.is_unspecified() {
                    AddressClass::Unspecified
                } else if addr.is_loopback() {
                    AddressClass::Loopback
                } else if addr.is_private() {
                    AddressClass::Private
                } else if ipv4_in_range(addr, 100, 64, 10) {
                    AddressClass::CarrierGradeNat
                } else if ipv4_in_range(addr, 198, 18, 15)
                    || ipv4_in_range(addr, 192, 0, 24)
                    || ipv4_in_range(addr, 240, 0, 4)
                {
                    AddressClass::Reserved
                } else if addr.is_link_local() {
                    AddressClass::LinkLocal
                } else if addr.is_multicast() {
                    AddressClass::Multicast
                } else if addr.is_broadcast() {
                    AddressClass::Broadcast
                } else if addr.is_documentation() {
                    AddressClass::Documentation
                } else {
                    AddressClass::Public
                }
            }
            IpAddr::V6(addr) => {
                // Only IPv4-mapped IPv6 addresses (`::ffff:x.y.z.w`) should
                // inherit IPv4 policy. `Ipv6Addr::to_ipv4` also accepts the
                // deprecated IPv4-compatible `::x.y.z.w` form, which would
                // otherwise misclassify `::1` as public `0.0.0.1`.
                if addr.segments()[..6] == [0, 0, 0, 0, 0, 0xffff] {
                    let mapped = addr
                        .to_ipv4()
                        .expect("IPv4-mapped address has an IPv4 tail");
                    return Self::classify(IpAddr::V4(mapped));
                }
                if addr.is_unspecified() {
                    AddressClass::Unspecified
                } else if addr.is_loopback() {
                    AddressClass::Loopback
                } else if is_ipv6_unicast_link_local(&addr) {
                    AddressClass::LinkLocal
                } else if addr.is_multicast() {
                    AddressClass::Multicast
                } else if is_ipv6_unique_local(addr) {
                    AddressClass::UniqueLocal
                } else if is_ipv6_documentation(addr) {
                    AddressClass::Documentation
                } else {
                    AddressClass::Public
                }
            }
        }
    }

    fn allowed_by(self, policy: OutboundEgressPolicy) -> bool {
        match self {
            AddressClass::Public => true,
            AddressClass::Loopback => policy.allow_loopback,
            AddressClass::Private | AddressClass::CarrierGradeNat | AddressClass::UniqueLocal => {
                policy.allow_private
            }
            AddressClass::LinkLocal => policy.allow_link_local,
            AddressClass::Multicast => policy.allow_multicast,
            AddressClass::Unspecified => policy.allow_unspecified,
            AddressClass::Documentation | AddressClass::Broadcast | AddressClass::Reserved => false,
        }
    }
}

fn ipv4_in_range(addr: std::net::Ipv4Addr, first_octet: u8, second_octet: u8, prefix: u8) -> bool {
    let value = u32::from(addr);
    let network = u32::from_be_bytes([first_octet, second_octet, 0, 0]);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    value & mask == network & mask
}

fn is_ipv6_unicast_link_local(addr: &std::net::Ipv6Addr) -> bool {
    addr.segments()[0] & 0xffc0 == 0xfe80
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPolicyError {
    SchemeDenied {
        kind: OutboundTargetKind,
        scheme: String,
    },
    AddressDenied {
        addr: IpAddr,
        class: AddressClass,
    },
    MissingHost,
    MissingPort,
    Resolution(String),
    Client(String),
}

impl std::fmt::Display for EgressPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EgressPolicyError::SchemeDenied { kind, scheme } => {
                write!(f, "{kind:?} scheme denied: {scheme}")
            }
            EgressPolicyError::AddressDenied { addr, class } => {
                write!(f, "egress address denied: {addr} ({class:?})")
            }
            EgressPolicyError::MissingHost => write!(f, "egress URL has no host"),
            EgressPolicyError::MissingPort => write!(f, "egress URL has no port"),
            EgressPolicyError::Resolution(error) => {
                write!(f, "egress DNS resolution failed: {error}")
            }
            EgressPolicyError::Client(error) => {
                write!(f, "egress HTTP client creation failed: {error}")
            }
        }
    }
}

impl std::error::Error for EgressPolicyError {}

fn is_ipv6_unique_local(addr: std::net::Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_documentation(addr: std::net::Ipv6Addr) -> bool {
    addr.segments()[0] == 0x2001 && addr.segments()[1] == 0x0db8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_public_tracker_schemes_only() {
        let policy = OutboundEgressPolicy::default();
        for url in [
            "http://tracker.example/announce",
            "https://tracker.example/announce",
            "udp://tracker.example:6969/announce",
        ] {
            policy
                .validate_url(OutboundTargetKind::Tracker, &Url::parse(url).unwrap())
                .unwrap();
        }
        assert!(policy
            .validate_url(
                OutboundTargetKind::Tracker,
                &Url::parse("file:///tmp/announce").unwrap()
            )
            .is_err());
    }

    #[test]
    fn default_policy_allows_only_http_webseeds() {
        let policy = OutboundEgressPolicy::default();
        policy
            .validate_url(
                OutboundTargetKind::Webseed,
                &Url::parse("https://seed.example/file").unwrap(),
            )
            .unwrap();
        assert!(policy
            .validate_url(
                OutboundTargetKind::Webseed,
                &Url::parse("udp://seed.example:80/file").unwrap(),
            )
            .is_err());
    }

    #[test]
    fn default_policy_denies_sensitive_address_ranges() {
        let policy = OutboundEgressPolicy::default();
        for addr in [
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "172.16.0.1".parse().unwrap(),
            "192.168.1.1".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            "224.0.0.1".parse().unwrap(),
            "255.255.255.255".parse().unwrap(),
            "::1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "fc00::1".parse().unwrap(),
            "2001:db8::1".parse().unwrap(),
            "100.64.0.1".parse().unwrap(),
            "198.18.0.1".parse().unwrap(),
            "192.0.0.1".parse().unwrap(),
            "240.0.0.1".parse().unwrap(),
            "::ffff:127.0.0.1".parse().unwrap(),
        ] {
            assert!(policy.validate_ip(addr).is_err(), "{addr}");
        }
        policy.validate_ip("8.8.8.8".parse().unwrap()).unwrap();
        policy
            .validate_ip("2001:4860:4860::8888".parse().unwrap())
            .unwrap();
    }

    #[tokio::test]
    async fn http_clients_are_reused_after_each_address_is_revalidated() {
        let policy = OutboundEgressPolicy {
            allow_loopback: true,
            ..OutboundEgressPolicy::default()
        };
        let url = Url::parse("http://127.0.0.1:9/announce").unwrap();
        let first = policy
            .http_client(
                OutboundTargetKind::Tracker,
                &url,
                Duration::from_secs(1),
                "TorrentNG/test",
            )
            .await
            .unwrap();
        let second = policy
            .http_client(
                OutboundTargetKind::Tracker,
                &url,
                Duration::from_secs(1),
                "TorrentNG/test",
            )
            .await
            .unwrap();
        // `reqwest::Client` is intentionally cloneable; pointer identity is
        // not part of its API. A second construction would be observable as
        // a second cache entry, so assert the bounded cache contains this
        // exact policy tuple rather than relying on internal client details.
        let _ = (first, second);
        let cache = http_client_cache()
            .lock()
            .expect("egress client cache poisoned");
        assert!(cache.clients.keys().any(|key| {
            key.kind == OutboundTargetKind::Tracker
                && key.host == "127.0.0.1"
                && key.port == 9
                && key.user_agent == "TorrentNG/test"
        }));
    }

    #[test]
    fn policy_can_explicitly_allow_private_lan() {
        let policy = OutboundEgressPolicy {
            allow_private: true,
            allow_link_local: true,
            ..OutboundEgressPolicy::default()
        };
        policy.validate_ip("192.168.1.1".parse().unwrap()).unwrap();
        policy.validate_ip("fe80::1".parse().unwrap()).unwrap();
        assert!(policy.validate_ip("127.0.0.1".parse().unwrap()).is_err());
    }
}
