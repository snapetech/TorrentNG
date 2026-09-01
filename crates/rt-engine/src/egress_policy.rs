use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use rt_config::TrackerConfig;
use tokio::net::lookup_host;
use tokio::time::timeout;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundTargetKind {
    Tracker,
    Webseed,
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
                _ => Err(EgressPolicyError::SchemeDenied {
                    kind,
                    scheme: scheme.to_owned(),
                }),
            },
            OutboundTargetKind::Webseed => match scheme {
                "http" if self.allow_http_webseeds => Ok(()),
                "https" if self.allow_https_webseeds => Ok(()),
                _ => Err(EgressPolicyError::SchemeDenied {
                    kind,
                    scheme: scheme.to_owned(),
                }),
            },
        }
    }

    pub fn validate_ip(&self, addr: IpAddr) -> Result<(), EgressPolicyError> {
        let class = AddressClass::classify(addr);
        if class.allowed_by(*self) {
            Ok(())
        } else {
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
        let host = url
            .host_str()
            .ok_or(EgressPolicyError::MissingHost)?
            .to_owned();
        let port = url
            .port_or_known_default()
            .ok_or(EgressPolicyError::MissingPort)?;
        let addresses = timeout(resolve_timeout, lookup_host((host.as_str(), port)))
            .await
            .map_err(|_| EgressPolicyError::Resolution("DNS resolution timed out".to_owned()))?
            .map_err(|error| EgressPolicyError::Resolution(error.to_string()))?
            .collect::<Vec<_>>();
        if addresses.is_empty() {
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
        let host = url.host_str().ok_or(EgressPolicyError::MissingHost)?;
        let address = addresses
            .first()
            .copied()
            .ok_or_else(|| EgressPolicyError::Resolution("no validated address".to_owned()))?;
        reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(user_agent)
            .resolve(host, address)
            .build()
            .map_err(|error| EgressPolicyError::Client(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressClass {
    Public,
    Loopback,
    Private,
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
            AddressClass::Private | AddressClass::UniqueLocal => policy.allow_private,
            AddressClass::LinkLocal => policy.allow_link_local,
            AddressClass::Multicast => policy.allow_multicast,
            AddressClass::Unspecified => policy.allow_unspecified,
            AddressClass::Documentation | AddressClass::Broadcast => false,
        }
    }
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
        ] {
            assert!(policy.validate_ip(addr).is_err(), "{addr}");
        }
        policy.validate_ip("8.8.8.8".parse().unwrap()).unwrap();
        policy
            .validate_ip("2001:4860:4860::8888".parse().unwrap())
            .unwrap();
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
