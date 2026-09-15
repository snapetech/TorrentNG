//! Detached tracker transport and worker ownership.
//!
//! `TorrentTask` remains the ordering authority for tracker state, tier
//! failover, and peer admission.  This module owns the network futures and
//! their cancellation handles so a slow or broken tracker cannot occupy the
//! torrent actor while it is waiting on I/O.

use std::collections::HashMap;
use std::time::Duration;

use rt_metrics::{MemoryClass, MemoryLease, ResourceGovernor};
use rt_tracker::{
    to_http_scrape_url,
    udp::{UdpAnnounceRequest, UdpAnnounceResponse, UdpConnectRequest, UdpConnectResponse},
    AnnounceRequest, AnnounceResponse, InfoHash, ScrapeStats, TrackerError, TrackerEvent,
    MAX_TRACKER_PEERS, MAX_TRACKER_STATE_ID_BYTES, MAX_TRACKER_STATE_TEXT_BYTES,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::warn;
use url::Url;

use crate::egress_policy::{OutboundEgressPolicy, OutboundTargetKind};

const MAX_TRACKER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_TRACKER_UDP_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TRACKER_ANNOUNCES_IN_FLIGHT: usize = 8;
pub(crate) const STOPPED_TRACKER_ANNOUNCE_DEADLINE: Duration = Duration::from_secs(10);
// A parsed compact response can grow its peer vector once more when a
// response contains both `peers` and `peers6`; the actor then materializes a
// second address-only vector before connecting. Reserve for that peak while
// retaining the response across the bounded worker-result channel.
const TRACKER_RESPONSE_PEER_VECTOR_CAPACITY_MULTIPLIER: usize = 2;

pub(crate) type TrackerKey = (usize, usize);

/// URI schemes are case-insensitive. Dispatch must inspect the scheme with
/// the same rule; routing `UDP://...` through the HTTP client makes a valid
/// UDP tracker fail before the UDP announce code gets a chance to parse it.
pub(crate) fn is_udp_tracker_url(tracker_url: &str) -> bool {
    tracker_url
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("udp://"))
}

/// Encode the daemon's bounded tracker peer ceiling in the tracker protocol's
/// fixed-width field without allowing a 64-bit value to wrap on the wire.
pub(crate) fn protocol_numwant(max_peers: usize) -> u32 {
    u32::try_from(max_peers.min(MAX_TRACKER_PEERS)).unwrap_or(u32::MAX)
}

#[derive(Clone)]
pub(crate) struct TrackerAnnounceContext {
    pub(crate) info_hash: [u8; 20],
    pub(crate) uploaded: u64,
    pub(crate) downloaded: u64,
    pub(crate) left: u64,
    pub(crate) listen_port: u16,
    pub(crate) http_timeout: Duration,
    pub(crate) udp_timeout: Duration,
    pub(crate) numwant: u32,
    pub(crate) egress_policy: OutboundEgressPolicy,
    pub(crate) resources: ResourceGovernor,
}

pub(crate) struct TrackerAnnounceSpec {
    pub(crate) key: TrackerKey,
    pub(crate) url: String,
    pub(crate) tracker_id: Option<Vec<u8>>,
    pub(crate) event: TrackerEvent,
}

pub(crate) struct TrackerAnnounceResult {
    pub(crate) key: TrackerKey,
    pub(crate) generation: u64,
    pub(crate) url: String,
    pub(crate) event: TrackerEvent,
    pub(crate) response: Result<AnnounceResponse, TrackerError>,
    pub(crate) scrape: Option<ScrapeStats>,
    pub(crate) _response_memory_lease: Option<MemoryLease>,
}

pub(crate) struct LeasedAnnounceResponse {
    pub(crate) response: AnnounceResponse,
    pub(crate) _memory_lease: MemoryLease,
}

/// Owns detached announce/scrape futures for one torrent actor.
pub(crate) struct TrackerWorkers {
    result_tx: mpsc::Sender<TrackerAnnounceResult>,
    result_rx: mpsc::Receiver<TrackerAnnounceResult>,
    inflight: HashMap<TrackerKey, tokio::task::AbortHandle>,
    generation: u64,
}

impl TrackerWorkers {
    pub(crate) fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel(128);
        Self {
            result_tx,
            result_rx,
            inflight: HashMap::new(),
            generation: 0,
        }
    }

    pub(crate) fn available(&self) -> usize {
        MAX_TRACKER_ANNOUNCES_IN_FLIGHT.saturating_sub(self.inflight.len())
    }

    pub(crate) fn contains(&self, key: TrackerKey) -> bool {
        self.inflight.contains_key(&key)
    }

    pub(crate) fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    pub(crate) fn start(
        &mut self,
        specs: impl IntoIterator<Item = TrackerAnnounceSpec>,
        context: TrackerAnnounceContext,
    ) {
        for spec in specs {
            if self.inflight.len() >= MAX_TRACKER_ANNOUNCES_IN_FLIGHT {
                break;
            }
            if self.inflight.contains_key(&spec.key) {
                continue;
            }

            let TrackerAnnounceSpec {
                key,
                url,
                tracker_id,
                event,
            } = spec;
            let generation = self.generation;
            let result_tx = self.result_tx.clone();
            let worker_context = context.clone();
            let worker_url = url.clone();
            let worker = tokio::spawn(async move {
                let response =
                    announce_tracker(&worker_context, &worker_url, event, tracker_id.as_deref())
                        .await;
                // UDP trackers have no HTTP scrape endpoint. Trying to pass
                // their URL through the HTTP egress policy causes a failed
                // client/DNS attempt after every successful announce.
                let scrape = if response.is_ok() && !is_udp_tracker_url(&worker_url) {
                    scrape_tracker(&worker_context, &worker_url).await.ok()
                } else {
                    None
                };
                let (response, response_memory_lease) = match response {
                    Ok(response) => (Ok(response.response), Some(response._memory_lease)),
                    Err(error) => (Err(error), None),
                };
                if result_tx
                    .send(TrackerAnnounceResult {
                        key,
                        generation,
                        url: worker_url,
                        event,
                        response,
                        scrape,
                        _response_memory_lease: response_memory_lease,
                    })
                    .await
                    .is_err()
                {
                    warn!(
                        component = "tracker",
                        operation = "announce_worker",
                        result = "actor_gone",
                        "tracker announce result discarded because the torrent actor stopped"
                    );
                }
            });
            self.inflight.insert(key, worker.abort_handle());
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<TrackerAnnounceResult> {
        self.result_rx.recv().await
    }

    pub(crate) fn complete(&mut self, key: TrackerKey, generation: u64) {
        // A cancelled worker must not remove a new worker that reused the
        // same (tier, tracker) key after a session restart.
        if generation == self.generation {
            self.inflight.remove(&key);
        }
    }

    pub(crate) fn has_inflight_tier(&self, tier_idx: usize) -> bool {
        self.inflight
            .keys()
            .any(|(pending_tier, _)| *pending_tier == tier_idx)
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        for abort in self.inflight.drain().map(|(_, abort)| abort) {
            abort.abort();
        }
        while self.result_rx.try_recv().is_ok() {}
    }
}

impl Drop for TrackerWorkers {
    fn drop(&mut self) {
        // A torrent actor can exit through an early recovery/shutdown path
        // that does not get back to its normal command branch. Do not leave
        // network futures alive merely because their result receiver was
        // dropped.
        for abort in self.inflight.drain().map(|(_, abort)| abort) {
            abort.abort();
        }
    }
}

pub(crate) async fn announce_tracker(
    context: &TrackerAnnounceContext,
    tracker_url: &str,
    event: TrackerEvent,
    tracker_id: Option<&[u8]>,
) -> Result<LeasedAnnounceResponse, TrackerError> {
    if is_udp_tracker_url(tracker_url) {
        announce_udp(context, tracker_url, event).await
    } else {
        announce_http(context, tracker_url, event, tracker_id).await
    }
}

async fn announce_http(
    context: &TrackerAnnounceContext,
    tracker_url: &str,
    event: TrackerEvent,
    tracker_id: Option<&[u8]>,
) -> Result<LeasedAnnounceResponse, TrackerError> {
    let tracker =
        Url::parse(tracker_url).map_err(|error| TrackerError::InvalidUrl(error.to_string()))?;
    let user_agent = crate::peer_id::user_agent();
    let client = context
        .egress_policy
        .http_client(
            OutboundTargetKind::Tracker,
            &tracker,
            context.http_timeout,
            &user_agent,
        )
        .await
        .map_err(|error| TrackerError::Network(error.to_string()))?;

    let req = AnnounceRequest {
        info_hash: InfoHash::V1(context.info_hash),
        peer_id: crate::peer_id::our_peer_id(),
        port: context.listen_port,
        uploaded: context.uploaded,
        downloaded: context.downloaded,
        left: context.left,
        event,
        compact: true,
        numwant: Some(context.numwant),
    };
    let url = req.to_http_query_with_tracker_id(tracker_url, tracker_id)?;
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            TrackerError::Timeout
        } else {
            TrackerError::Network(e.to_string())
        }
    })?;
    if !response.status().is_success() {
        return Err(TrackerError::Http {
            status: response.status().as_u16(),
        });
    }
    let body = bounded_response_body_with_memory_and_extra(
        response,
        MAX_TRACKER_RESPONSE_BYTES,
        tracker_response_output_memory_bytes(tracker_peer_limit(context)),
        &context.resources,
    )
    .await?;
    let response =
        AnnounceResponse::parse_with_peer_limit(&body.bytes, tracker_peer_limit(context))?;
    Ok(LeasedAnnounceResponse {
        response,
        _memory_lease: body._lease,
    })
}

async fn announce_udp(
    context: &TrackerAnnounceContext,
    tracker_url: &str,
    event: TrackerEvent,
) -> Result<LeasedAnnounceResponse, TrackerError> {
    let url = Url::parse(tracker_url).map_err(|e| TrackerError::InvalidUrl(e.to_string()))?;
    let mut addrs = context
        .egress_policy
        .resolve_and_validate(OutboundTargetKind::Tracker, &url, context.udp_timeout)
        .await
        .map_err(|error| TrackerError::Network(error.to_string()))?;
    let tracker_addr = addrs
        .drain(..)
        .next()
        .ok_or_else(|| TrackerError::Network("no tracker address resolved".to_owned()))?;

    let bind_addr = if tracker_addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    socket
        .connect(tracker_addr)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let connect = UdpConnectRequest::new();
    socket
        .send(&connect.encode())
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let response_lease = reserve_tracker_response_bytes(
        &context.resources,
        MAX_TRACKER_UDP_RESPONSE_BYTES.saturating_add(tracker_response_output_memory_bytes(
            tracker_peer_limit(context),
        )),
    )?;
    let mut buf = vec![0u8; MAX_TRACKER_UDP_RESPONSE_BYTES];
    let n = tokio::time::timeout(context.udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let connect_resp = UdpConnectResponse::parse(&buf[..n])?;
    if connect_resp.transaction_id != connect.transaction_id {
        return Err(TrackerError::Udp("connect transaction id mismatch".into()));
    }

    let req = AnnounceRequest {
        info_hash: InfoHash::V1(context.info_hash),
        peer_id: crate::peer_id::our_peer_id(),
        port: context.listen_port,
        uploaded: context.uploaded,
        downloaded: context.downloaded,
        left: context.left,
        event,
        compact: true,
        numwant: Some(context.numwant),
    };
    let announce = UdpAnnounceRequest::new(connect_resp.connection_id, req);
    let encoded = announce.encode()?;
    socket
        .send(&encoded)
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;

    let n = tokio::time::timeout(context.udp_timeout, socket.recv(&mut buf))
        .await
        .map_err(|_| TrackerError::Timeout)?
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let announce_resp =
        UdpAnnounceResponse::parse_with_peer_limit(&buf[..n], tracker_peer_limit(context))?;
    if announce_resp.transaction_id != announce.transaction_id {
        return Err(TrackerError::Udp("announce transaction id mismatch".into()));
    }

    Ok(LeasedAnnounceResponse {
        response: AnnounceResponse {
            interval: announce_resp.interval,
            min_interval: None,
            peers: announce_resp.peers,
            tracker_id: None,
            warning_message: None,
            complete: Some(announce_resp.seeders),
            incomplete: Some(announce_resp.leechers),
        },
        _memory_lease: response_lease,
    })
}

fn tracker_peer_limit(context: &TrackerAnnounceContext) -> usize {
    (context.numwant as usize).min(MAX_TRACKER_PEERS)
}

pub(crate) fn tracker_response_output_memory_bytes(max_peers: usize) -> usize {
    let peer_bytes = std::mem::size_of::<rt_tracker::Peer>()
        .saturating_mul(TRACKER_RESPONSE_PEER_VECTOR_CAPACITY_MULTIPLIER)
        .saturating_add(std::mem::size_of::<std::net::SocketAddr>());
    max_peers
        .saturating_mul(peer_bytes)
        .saturating_add(MAX_TRACKER_STATE_ID_BYTES)
        .saturating_add(MAX_TRACKER_STATE_TEXT_BYTES)
}

async fn scrape_tracker(
    context: &TrackerAnnounceContext,
    tracker_url: &str,
) -> Result<ScrapeStats, TrackerError> {
    let tracker =
        Url::parse(tracker_url).map_err(|error| TrackerError::InvalidUrl(error.to_string()))?;
    let user_agent = crate::peer_id::user_agent();
    let client = context
        .egress_policy
        .http_client(
            OutboundTargetKind::Tracker,
            &tracker,
            context.http_timeout,
            &user_agent,
        )
        .await
        .map_err(|error| TrackerError::Network(error.to_string()))?;
    let url = to_http_scrape_url(tracker_url, InfoHash::V1(context.info_hash))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| TrackerError::Network(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(TrackerError::Http {
            status: status.as_u16(),
        });
    }
    let body =
        bounded_response_body_with_memory(resp, MAX_TRACKER_RESPONSE_BYTES, &context.resources)
            .await?;
    ScrapeStats::parse(&body.bytes, &context.info_hash)
}

pub(crate) struct LeasedResponseBody {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _lease: MemoryLease,
}

pub(crate) fn reserve_tracker_response_bytes(
    resources: &ResourceGovernor,
    bytes: usize,
) -> Result<MemoryLease, TrackerError> {
    resources
        .try_acquire(
            MemoryClass::TrackerPeers,
            u64::try_from(bytes).unwrap_or(u64::MAX),
        )
        .ok_or_else(|| TrackerError::Network("tracker response memory budget exhausted".to_owned()))
}

pub(crate) async fn bounded_response_body_with_memory(
    response: reqwest::Response,
    max_bytes: usize,
    resources: &ResourceGovernor,
) -> Result<LeasedResponseBody, TrackerError> {
    bounded_response_body_with_memory_and_extra(response, max_bytes, 0, resources).await
}

pub(crate) async fn bounded_response_body_with_memory_and_extra(
    response: reqwest::Response,
    max_bytes: usize,
    extra_bytes: usize,
    resources: &ResourceGovernor,
) -> Result<LeasedResponseBody, TrackerError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(TrackerError::ParseError(format!(
            "response exceeds {max_bytes} byte limit"
        )));
    }
    // Reserve the full allowed body size, not merely Content-Length. A peer
    // can omit or misstate that header, while the streaming reader still
    // permits any body up to `max_bytes`; the lease must cover that worst
    // case so the governor cannot be bypassed by an inconsistent response.
    // `extra_bytes` covers bounded parser/consumer output that must overlap
    // the body while the response is being decoded.
    let lease = reserve_tracker_response_bytes(resources, max_bytes.saturating_add(extra_bytes))?;
    let bytes = bounded_response_body(response, max_bytes).await?;
    Ok(LeasedResponseBody {
        bytes,
        _lease: lease,
    })
}

/// Read an HTTP response with a hard byte ceiling before parsing it.
pub(crate) async fn bounded_response_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, TrackerError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(TrackerError::ParseError(format!(
            "response exceeds {max_bytes} byte limit"
        )));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .map(|length| length.min(max_bytes as u64) as usize)
            .unwrap_or_default(),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| TrackerError::Network(error.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(TrackerError::ParseError(format!(
                "response exceeds {max_bytes} byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{
        is_udp_tracker_url, protocol_numwant, reserve_tracker_response_bytes, TrackerWorkers,
        MAX_TRACKER_ANNOUNCES_IN_FLIGHT,
    };
    use rt_metrics::{MemoryClass, ResourceGovernor, ResourceGovernorConfig, MEMORY_CLASS_COUNT};
    use rt_tracker::MAX_TRACKER_PEERS;

    #[test]
    fn tracker_scheme_dispatch_is_case_insensitive() {
        assert!(is_udp_tracker_url("udp://tracker.example:6969/announce"));
        assert!(is_udp_tracker_url("UDP://tracker.example:6969/announce"));
        assert!(is_udp_tracker_url("UdP://tracker.example:6969/announce"));
        assert!(!is_udp_tracker_url("http://tracker.example/announce"));
        assert!(!is_udp_tracker_url(
            "udp-tracker://tracker.example/announce"
        ));
    }

    #[test]
    fn tracker_worker_budget_is_explicit_and_bounded() {
        let workers = TrackerWorkers::new();
        assert_eq!(workers.available(), MAX_TRACKER_ANNOUNCES_IN_FLIGHT);
        assert!(!workers.contains((0, 0)));
    }

    #[test]
    fn protocol_peer_limit_does_not_wrap() {
        assert_eq!(protocol_numwant(200), 200);
        assert_eq!(protocol_numwant(usize::MAX), MAX_TRACKER_PEERS as u32);
    }

    #[test]
    fn tracker_response_buffer_reservation_is_shared_and_released() {
        let mut class_caps_bytes = [0; MEMORY_CLASS_COUNT];
        class_caps_bytes[MemoryClass::TrackerPeers as usize] = 64;
        let governor = ResourceGovernor::new(ResourceGovernorConfig {
            total_cap_bytes: 64,
            class_caps_bytes,
            pressure_constrained_pct: 75,
            pressure_critical_pct: 90,
        });

        let lease = reserve_tracker_response_bytes(&governor, 64).unwrap();
        assert!(reserve_tracker_response_bytes(&governor, 1).is_err());
        drop(lease);
        assert!(reserve_tracker_response_bytes(&governor, 64).is_ok());
    }
}
