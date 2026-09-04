//! Detached tracker transport and worker ownership.
//!
//! `TorrentTask` remains the ordering authority for tracker state, tier
//! failover, and peer admission.  This module owns the network futures and
//! their cancellation handles so a slow or broken tracker cannot occupy the
//! torrent actor while it is waiting on I/O.

use std::collections::HashMap;
use std::time::Duration;

use rt_tracker::{
    to_http_scrape_url,
    udp::{UdpAnnounceRequest, UdpAnnounceResponse, UdpConnectRequest, UdpConnectResponse},
    AnnounceRequest, AnnounceResponse, InfoHash, ScrapeStats, TrackerError, TrackerEvent,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::warn;
use url::Url;

use crate::egress_policy::{OutboundEgressPolicy, OutboundTargetKind};

const MAX_TRACKER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_TRACKER_ANNOUNCES_IN_FLIGHT: usize = 8;
pub(crate) const STOPPED_TRACKER_ANNOUNCE_DEADLINE: Duration = Duration::from_secs(10);

pub(crate) type TrackerKey = (usize, usize);

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
                let scrape = if response.is_ok() {
                    scrape_tracker(&worker_context, &worker_url).await.ok()
                } else {
                    None
                };
                if result_tx
                    .send(TrackerAnnounceResult {
                        key,
                        generation,
                        url: worker_url,
                        event,
                        response,
                        scrape,
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
) -> Result<AnnounceResponse, TrackerError> {
    if tracker_url.starts_with("udp://") {
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
) -> Result<AnnounceResponse, TrackerError> {
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
    let bytes = bounded_response_body(response, MAX_TRACKER_RESPONSE_BYTES).await?;
    AnnounceResponse::parse(&bytes)
}

async fn announce_udp(
    context: &TrackerAnnounceContext,
    tracker_url: &str,
    event: TrackerEvent,
) -> Result<AnnounceResponse, TrackerError> {
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

    let mut buf = vec![0u8; 64 * 1024];
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
    let announce_resp = UdpAnnounceResponse::parse(&buf[..n])?;
    if announce_resp.transaction_id != announce.transaction_id {
        return Err(TrackerError::Udp("announce transaction id mismatch".into()));
    }

    Ok(AnnounceResponse {
        interval: announce_resp.interval,
        min_interval: None,
        peers: announce_resp.peers,
        tracker_id: None,
        warning_message: None,
        complete: Some(announce_resp.seeders),
        incomplete: Some(announce_resp.leechers),
    })
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
    let body = bounded_response_body(resp, MAX_TRACKER_RESPONSE_BYTES).await?;
    ScrapeStats::parse(&body, &context.info_hash)
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
            "response exceeds {} byte limit",
            max_bytes
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
                "response exceeds {} byte limit",
                max_bytes
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{TrackerWorkers, MAX_TRACKER_ANNOUNCES_IN_FLIGHT};

    #[test]
    fn tracker_worker_budget_is_explicit_and_bounded() {
        let workers = TrackerWorkers::new();
        assert_eq!(workers.available(), MAX_TRACKER_ANNOUNCES_IN_FLIGHT);
        assert!(!workers.contains((0, 0)));
    }
}
