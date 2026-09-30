use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, MutexGuard, OnceLock,
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
const MAX_RESOLVED_ADDRESSES: usize = 64;

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

fn lock_http_client_cache(cache: &Mutex<HttpClientCache>) -> MutexGuard<'_, HttpClientCache> {
    match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            // Every caller re-resolves and validates DNS before consulting
            // this cache, so cached clients are disposable transport state.
            // Drop any partially updated entries and rebuild on demand.
            let mut guard = poisoned.into_inner();
            *guard = HttpClientCache::default();
            cache.clear_poison();
            guard
        }
    }
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

    /// Validate an outbound BitTorrent peer destination using the same address
    /// classes as tracker and webseed egress.
    pub fn validate_peer_addr(&self, addr: SocketAddr) -> Result<(), EgressPolicyError> {
        self.validate_socket_addr(addr)
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
        let resolved = timeout(resolve_timeout, lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Resolution("DNS resolution timed out".to_owned())
            })?
            .map_err(|error| {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Resolution(error.to_string())
            })?;
        let addresses = match bounded_resolved_addresses(resolved) {
            Ok(addresses) => addresses,
            Err(error) => {
                RESOLUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
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
        if let Some(client) = lock_http_client_cache(http_client_cache()).get(&key) {
            return Ok(client);
        }
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            // Environment-configured proxies resolve the destination outside
            // this policy boundary, bypassing both address validation and the
            // pinned resolver entry above. Tracker/webseed requests therefore
            // always connect directly to the validated address.
            .no_proxy()
            .user_agent(user_agent)
            .resolve(host, address)
            .build()
            .map_err(|error| {
                CLIENT_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
                EgressPolicyError::Client(error.to_string())
            })?;
        let mut cache = lock_http_client_cache(http_client_cache());
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

fn bounded_resolved_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, EgressPolicyError> {
    let addresses = addresses
        .into_iter()
        .take(MAX_RESOLVED_ADDRESSES.saturating_add(1))
        .collect::<Vec<_>>();
    if addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err(EgressPolicyError::Resolution(format!(
            "DNS returned more than {MAX_RESOLVED_ADDRESSES} addresses"
        )));
    }
    Ok(addresses)
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
                let octets = addr.octets();
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
                    || octets[0] == 0
                    || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
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
                let segments = addr.segments();
                if segments[..6] == [0, 0, 0, 0, 0, 0xffff] {
                    let mapped = addr
                        .to_ipv4()
                        .expect("IPv4-mapped address has an IPv4 tail");
                    return Self::classify(IpAddr::V4(mapped));
                }
                if let Some(translated) = ipv6_well_known_nat64_ipv4(addr) {
                    return Self::classify(IpAddr::V4(translated));
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
                } else if is_ipv6_reserved_prefix(addr) || !is_ipv6_global_unicast(addr) {
                    AddressClass::Reserved
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
    let segments = addr.segments();
    (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
}

fn ipv6_well_known_nat64_ipv4(addr: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = addr.segments();
    (segments[..6] == [0x0064, 0xff9b, 0, 0, 0, 0])
        .then(|| Ipv4Addr::from((u32::from(segments[6]) << 16) | u32::from(segments[7])))
}

fn is_ipv6_reserved_prefix(addr: Ipv6Addr) -> bool {
    let segments = addr.segments();
    // IPv4-compatible addresses are deprecated. The IPv4-mapped form is
    // handled separately so it inherits the corresponding IPv4 policy.
    segments[..6] == [0, 0, 0, 0, 0, 0]
        // IANA's 2001::/23 protocol-assignment block and 6to4 both carry
        // transition/special-use semantics, not ordinary public unicast.
        || (segments[0] == 0x2001 && segments[1] & 0xfe00 == 0)
        || segments[0] == 0x2002
}

fn is_ipv6_global_unicast(addr: Ipv6Addr) -> bool {
    // IANA's 2000::/3 is the assignable GUA space, not a blanket assertion
    // that every address in it is allocated. Keep the current assigned
    // prefixes and fail closed on the unlisted ranges reserved for future
    // allocation. Registry snapshot: 2025-10-10.
    const ALLOCATED_PREFIXES: &[(u16, u16, u32)] = &[
        (0x2001, 0x0000, 23),
        (0x2001, 0x0200, 23),
        (0x2001, 0x0400, 23),
        (0x2001, 0x0600, 23),
        (0x2001, 0x0800, 22),
        (0x2001, 0x0c00, 23),
        (0x2001, 0x0e00, 23),
        (0x2001, 0x1200, 23),
        (0x2001, 0x1400, 22),
        (0x2001, 0x1800, 23),
        (0x2001, 0x1a00, 23),
        (0x2001, 0x1c00, 22),
        (0x2001, 0x2000, 19),
        (0x2001, 0x4000, 23),
        (0x2001, 0x4200, 23),
        (0x2001, 0x4400, 23),
        (0x2001, 0x4600, 23),
        (0x2001, 0x4800, 23),
        (0x2001, 0x4a00, 23),
        (0x2001, 0x4c00, 23),
        (0x2001, 0x5000, 20),
        (0x2001, 0x8000, 19),
        (0x2001, 0xa000, 20),
        (0x2001, 0xb000, 20),
        (0x2003, 0x0000, 18),
        (0x2400, 0x0000, 12),
        (0x2410, 0x0000, 12),
        (0x2600, 0x0000, 12),
        (0x2610, 0x0000, 23),
        (0x2620, 0x0000, 23),
        (0x2630, 0x0000, 12),
        (0x2800, 0x0000, 12),
        (0x2a00, 0x0000, 12),
        (0x2a10, 0x0000, 12),
        (0x2c00, 0x0000, 12),
    ];
    let address = u128::from(addr);
    ALLOCATED_PREFIXES
        .iter()
        .any(|(first, second, prefix_len)| {
            let network = (u128::from(*first) << 112) | (u128::from(*second) << 96);
            let mask = u128::MAX << (128 - *prefix_len);
            address & mask == network & mask
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::process::Command;
    use std::thread;
    use std::time::Instant;

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
            "0.0.0.1".parse().unwrap(),
            "192.88.99.1".parse().unwrap(),
            "240.0.0.1".parse().unwrap(),
            "::ffff:127.0.0.1".parse().unwrap(),
            "::8.8.8.8".parse().unwrap(),
            "100::1".parse().unwrap(),
            "2000::1".parse().unwrap(),
            "2001::1".parse().unwrap(),
            "2002:c0a8:0101::1".parse().unwrap(),
            "2003:8000::1".parse().unwrap(),
            "2004::1".parse().unwrap(),
            "2500::1".parse().unwrap(),
            "2d00::1".parse().unwrap(),
            "3fff::1".parse().unwrap(),
            "5f00::1".parse().unwrap(),
            "64:ff9b::a9fe:a9fe".parse().unwrap(),
            "64:ff9b:1::1".parse().unwrap(),
        ] {
            assert!(policy.validate_ip(addr).is_err(), "{addr}");
        }
        policy.validate_ip("8.8.8.8".parse().unwrap()).unwrap();
        policy
            .validate_ip("2001:4860:4860::8888".parse().unwrap())
            .unwrap();
        for addr in ["2003::1", "2404::1", "2410::1", "2630::1", "2c00::1"] {
            policy.validate_ip(addr.parse().unwrap()).unwrap();
        }
        policy
            .validate_ip("64:ff9b::808:808".parse().unwrap())
            .unwrap();
    }

    #[test]
    fn dns_answer_sets_are_bounded_before_collection() {
        let addresses = (0..=MAX_RESOLVED_ADDRESSES)
            .map(|index| SocketAddr::from(([192, 0, 2, (index % 254 + 1) as u8], 6881)));
        assert!(matches!(
            bounded_resolved_addresses(addresses),
            Err(EgressPolicyError::Resolution(message))
                if message.contains("more than 64 addresses")
        ));

        let addresses = (0..MAX_RESOLVED_ADDRESSES)
            .map(|index| SocketAddr::from(([192, 0, 2, (index % 254 + 1) as u8], 6881)));
        assert_eq!(bounded_resolved_addresses(addresses).unwrap().len(), 64);
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
        let cache = lock_http_client_cache(http_client_cache());
        assert!(cache.clients.keys().any(|key| {
            key.kind == OutboundTargetKind::Tracker
                && key.host == "127.0.0.1"
                && key.port == 9
                && key.user_agent == "TorrentNG/test"
        }));
    }

    #[test]
    fn poisoned_http_client_cache_discards_clients_and_recovers() {
        let cache = std::sync::Arc::new(Mutex::new(HttpClientCache::default()));
        cache.lock().unwrap().insert(
            HttpClientKey {
                kind: OutboundTargetKind::Tracker,
                host: "tracker.example".to_owned(),
                port: 80,
                address: "192.0.2.1:80".parse().unwrap(),
                timeout_millis: 1_000,
                user_agent: "TorrentNG/test".to_owned(),
            },
            reqwest::Client::new(),
        );

        let poisoner = std::sync::Arc::clone(&cache);
        assert!(std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the derived HTTP client cache");
        })
        .join()
        .is_err());
        assert!(cache.is_poisoned());

        assert!(lock_http_client_cache(&cache).clients.is_empty());
        assert!(!cache.is_poisoned());
    }

    #[tokio::test]
    async fn pinned_http_client_ignores_system_proxy_for_validated_destination() {
        const CHILD_FLAG: &str = "TNG_EGRESS_PROXY_TEST_CHILD";
        if std::env::var_os(CHILD_FLAG).is_some() {
            let url = std::env::var("TNG_EGRESS_PROXY_TEST_TARGET").unwrap();
            let url = Url::parse(&url).unwrap();
            let policy = OutboundEgressPolicy {
                allow_loopback: true,
                ..OutboundEgressPolicy::default()
            };
            let client = policy
                .http_client(
                    OutboundTargetKind::Tracker,
                    &url,
                    Duration::from_secs(2),
                    "TorrentNG/egress-proxy-test",
                )
                .await
                .unwrap();
            let response = client
                .get(url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            assert_eq!(response.text().await.unwrap(), "direct");
            return;
        }

        fn serve_once(
            listener: std::net::TcpListener,
            body: &'static str,
        ) -> thread::JoinHandle<bool> {
            listener.set_nonblocking(true).unwrap();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = [0u8; 2048];
                            let _ = stream.read(&mut request);
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(), body
                            );
                            stream.write_all(response.as_bytes()).unwrap();
                            return true;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return false;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return false,
                    }
                }
            })
        }

        let direct_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let direct_address = direct_listener.local_addr().unwrap();
        let proxy_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        proxy_listener.set_nonblocking(true).unwrap();
        let proxy_address = proxy_listener.local_addr().unwrap();
        let direct_server = serve_once(direct_listener, "direct");
        let target = format!("http://{direct_address}/announce");
        let proxy = format!("http://{proxy_address}");

        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "egress_policy::tests::pinned_http_client_ignores_system_proxy_for_validated_destination",
                "--nocapture",
            ])
            .env(CHILD_FLAG, "1")
            .env("TNG_EGRESS_PROXY_TEST_TARGET", &target)
            .env("HTTP_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("all_proxy", &proxy)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .unwrap();
        let direct_received = direct_server.join().unwrap();
        let proxy_received = match proxy_listener.accept() {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("failed to inspect mock proxy listener: {error}"),
        };

        assert!(
            output.status.success(),
            "isolated proxy test failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(direct_received, "request did not reach validated address");
        assert!(!proxy_received, "request escaped through environment proxy");
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

    #[test]
    fn peer_destinations_use_the_configured_address_policy() {
        let lan_peer: SocketAddr = "192.168.1.20:51413".parse().unwrap();
        let loopback_peer: SocketAddr = "127.0.0.1:51413".parse().unwrap();
        let public_peer: SocketAddr = "8.8.8.8:51413".parse().unwrap();
        let default = OutboundEgressPolicy::default();

        assert!(default.validate_peer_addr(lan_peer).is_err());
        assert!(default.validate_peer_addr(loopback_peer).is_err());
        assert!(default.validate_peer_addr(public_peer).is_ok());

        let lan_enabled = OutboundEgressPolicy {
            allow_private: true,
            allow_loopback: true,
            ..default
        };
        assert!(lan_enabled.validate_peer_addr(lan_peer).is_ok());
        assert!(lan_enabled.validate_peer_addr(loopback_peer).is_ok());
    }
}
